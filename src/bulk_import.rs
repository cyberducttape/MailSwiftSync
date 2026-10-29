//! Untrusted mailbox-list import and structural validation.
//!
//! Parsing is deliberately independent from egui state. This module owns the
//! worker thread, file limits, worksheet selection, row validation, and
//! conversion into import jobs; the UI owns only queue presentation and
//! result application.

use crate::{Form, SecretString};
use quick_xml::Reader as XmlReader;
use quick_xml::encoding::DecodingReader;
use quick_xml::events::Event as XmlEvent;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};
use std::thread;

mod rows;
mod xlsx;
mod xlsx_reference;
pub(crate) use rows::validate_headers;
use rows::{normalize_headers, record_values};
use xlsx::Workbook as XlsxWorkbook;

fn plaintext_secrets_allowed(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

fn plaintext_import_allowed(environment_opt_in: bool, per_import_acknowledged: bool) -> bool {
    environment_opt_in && per_import_acknowledged
}

pub(crate) fn plaintext_secret_import_enabled() -> bool {
    plaintext_secrets_allowed(
        std::env::var("MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS")
            .ok()
            .as_deref(),
    )
}

/// Immutable settings shared by all rows imported from one batch plan.
/// Mailbox-specific identities and credentials live in `BulkJob` deltas so a
/// large import does not clone the complete profile for every row.
#[derive(Clone)]
pub(crate) struct BatchPlanDefaults {
    pub(crate) profile: Arc<crate::Profile>,
    pub(crate) dry_run: bool,
}

#[derive(Clone)]
pub(crate) struct BulkJob {
    pub(crate) label: String,
    pub(crate) defaults: Arc<BatchPlanDefaults>,
    pub(crate) source_host: String,
    pub(crate) source_user: String,
    pub(crate) source_rate_tenant: Option<String>,
    pub(crate) source_credential_id: String,
    pub(crate) source_password: SecretString,
    pub(crate) destination_host: String,
    pub(crate) destination_user: String,
    pub(crate) destination_rate_tenant: Option<String>,
    pub(crate) destination_credential_id: String,
    pub(crate) destination_password: SecretString,
    pub(crate) profile_name: Option<String>,
    pub(crate) state: String,
}

impl BulkJob {
    pub(crate) fn defaults_from_form(form: &Form) -> Arc<BatchPlanDefaults> {
        let mut profile = form.profile.clone();
        profile.source_host.clear();
        profile.source_user.clear();
        profile.source_credential_id.clear();
        profile.destination_host.clear();
        profile.destination_user.clear();
        profile.destination_credential_id.clear();
        Arc::new(BatchPlanDefaults {
            profile: Arc::new(profile),
            dry_run: form.dry_run,
        })
    }

    pub(crate) fn from_form(label: String, form: Form, state: String) -> Self {
        let defaults = Self::defaults_from_form(&form);
        Self::from_form_with_defaults(label, form, state, defaults)
    }

    pub(crate) fn from_form_with_defaults(
        label: String,
        form: Form,
        state: String,
        defaults: Arc<BatchPlanDefaults>,
    ) -> Self {
        let source_host = form.profile.source_host.clone();
        let source_user = form.profile.source_user.clone();
        let source_rate_tenant = (form.profile.source_rate_tenant
            != defaults.profile.source_rate_tenant)
            .then(|| form.profile.source_rate_tenant.clone());
        let source_credential_id = form.profile.source_credential_id.clone();
        let source_password = form.source_password.clone();
        let destination_host = form.profile.destination_host.clone();
        let destination_user = form.profile.destination_user.clone();
        let destination_rate_tenant = (form.profile.destination_rate_tenant
            != defaults.profile.destination_rate_tenant)
            .then(|| form.profile.destination_rate_tenant.clone());
        let destination_credential_id = form.profile.destination_credential_id.clone();
        let destination_password = form.destination_password.clone();
        let profile_name = (!form.profile.name.is_empty()).then(|| form.profile.name.clone());
        Self {
            label,
            defaults,
            source_host,
            source_user,
            source_rate_tenant,
            source_credential_id,
            source_password,
            destination_host,
            destination_user,
            destination_rate_tenant,
            destination_credential_id,
            destination_password,
            profile_name,
            state,
        }
    }

    pub(crate) fn form(&self) -> Form {
        Form {
            profile: self.profile(),
            source_password: self.source_password.clone(),
            destination_password: self.destination_password.clone(),
            dry_run: self.defaults.dry_run,
        }
    }

    /// The row's effective profile. Unlike `form`, this never copies the
    /// session passwords.
    pub(crate) fn profile(&self) -> crate::Profile {
        let mut profile = (*self.defaults.profile).clone();
        profile.source_host = self.source_host.clone();
        profile.source_user = self.source_user.clone();
        if let Some(tenant) = &self.source_rate_tenant {
            profile.source_rate_tenant = tenant.clone();
        }
        profile.source_credential_id = self.source_credential_id.clone();
        profile.destination_host = self.destination_host.clone();
        profile.destination_user = self.destination_user.clone();
        if let Some(tenant) = &self.destination_rate_tenant {
            profile.destination_rate_tenant = tenant.clone();
        }
        profile.destination_credential_id = self.destination_credential_id.clone();
        if let Some(name) = &self.profile_name {
            profile.name = name.clone();
        }
        profile
    }
}

pub(crate) struct PendingSheetImport {
    #[allow(dead_code)]
    pub(crate) path: PathBuf,
    #[allow(dead_code)]
    pub(crate) sheets: Vec<String>,
    pub(crate) plaintext_acknowledged: bool,
}

pub(crate) enum BulkImportResult {
    #[allow(dead_code)]
    Jobs(Vec<BulkJob>),
    /// The parsed rows were written to the ledger as a new batch queue.
    Persisted(crate::controller::queue::ImportedQueue),
    #[allow(dead_code)]
    Workbook {
        path: PathBuf,
        sheets: Vec<String>,
        plaintext_acknowledged: bool,
    },
}

/// Start a bounded/validated mailbox-file import away from the egui thread.
/// The caller receives only the future result; file-format dispatch and
/// parser ownership remain with the import domain.
#[allow(dead_code)]
pub(crate) fn spawn_import(
    path: PathBuf,
    base: Form,
    plaintext_acknowledged: bool,
) -> Receiver<Result<BulkImportResult, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let result = if ext == "csv" {
            read_csv_with_ack(&path, &base, plaintext_acknowledged).map(BulkImportResult::Jobs)
        } else if ext == "xlsx" {
            workbook_sheets(&path).map(|sheets| BulkImportResult::Workbook {
                path,
                sheets,
                plaintext_acknowledged,
            })
        } else {
            Err("Choose a .csv or .xlsx file. Legacy .xls imports are disabled because they cannot be safely bounded before parsing.".into())
        };
        let _ = sender.send(result);
    });
    receiver
}

/// Start parsing one selected worksheet without blocking the UI.
pub(crate) fn spawn_sheet_import(
    path: PathBuf,
    base: Form,
    sheet_index: usize,
    plaintext_acknowledged: bool,
) -> Receiver<Result<BulkImportResult, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = read_sheet_with_ack(&path, &base, sheet_index, plaintext_acknowledged)
            .map(BulkImportResult::Jobs);
        let _ = sender.send(result);
    });
    receiver
}

#[cfg(test)]
pub(crate) fn read_csv(path: &Path, base: &Form) -> Result<Vec<BulkJob>, String> {
    read_csv_with_ack(path, base, false)
}

fn read_csv_with_ack(
    path: &Path,
    base: &Form,
    plaintext_acknowledged: bool,
) -> Result<Vec<BulkJob>, String> {
    let file = open_import_file(path)?;
    let mut input = BufReader::with_capacity(
        64 * 1024,
        file.take(crate::MAX_BULK_IMPORT_BYTES.saturating_add(1)),
    );
    let delimiter = detect_csv_delimiter(input.fill_buf().map_err(|error| error.to_string())?)?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_reader(input);
    let headers = normalize_headers(
        reader.headers().map_err(|error| error.to_string())?.iter(),
        crate::MAX_BULK_IMPORT_COLUMNS,
        "file",
    )?;
    let allow_plaintext_secrets =
        plaintext_import_allowed(plaintext_secret_import_enabled(), plaintext_acknowledged);
    validate_headers(&headers, allow_plaintext_secrets)?;
    let defaults = BulkJob::defaults_from_form(base);
    let mut jobs = Vec::new();
    for (index, record) in reader.records().enumerate() {
        if index >= crate::MAX_BULK_IMPORT_ROWS {
            return Err(format!(
                "The file exceeds the {}-row import limit.",
                crate::MAX_BULK_IMPORT_ROWS
            ));
        }
        let record = record.map_err(|error| error.to_string())?;
        let row_number = index + 2;
        let values = record_values(
            &headers,
            record.iter(),
            row_number,
            crate::MAX_BULK_IMPORT_CELL_BYTES,
        )?;
        jobs.push(job_from_values_with_defaults(
            values,
            base,
            row_number,
            allow_plaintext_secrets,
            Arc::clone(&defaults),
        )?);
    }
    if jobs.is_empty() {
        return Err("The file has no migration rows.".into());
    }
    if reader.into_inner().get_ref().limit() == 0 {
        return Err(format!(
            "The import file exceeds the {}-byte limit.",
            crate::MAX_BULK_IMPORT_BYTES
        ));
    }
    Ok(jobs)
}

/// Choose the field separator from the header line. Excel in many European
/// locales (German among them) saves "CSV" with semicolons, and some exports
/// use tabs. Each candidate parses the header with CSV quoting rules, so a
/// separator inside a quoted header field is not counted, and the candidate
/// whose header names the most required columns wins. Two candidates that
/// both name every required column but split the header differently are
/// refused rather than guessed; with no required column anywhere, comma is
/// used and header validation reports what is missing.
fn detect_csv_delimiter(start: &[u8]) -> Result<u8, String> {
    const REQUIRED: [&str; 4] = [
        "source_host",
        "source_user",
        "destination_host",
        "destination_user",
    ];
    let header_for = |delimiter: u8| {
        csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .from_reader(start)
            .headers()
            .map(|record| {
                record
                    .iter()
                    .map(|field| field.trim().to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
            .ok()
    };
    let mut best: Option<(u8, usize, Vec<String>)> = None;
    for delimiter in [b',', b';', b'\t'] {
        let Some(header) = header_for(delimiter) else {
            continue;
        };
        let recognized = REQUIRED
            .iter()
            .filter(|required| header.iter().any(|field| field == *required))
            .count();
        match &best {
            Some((chosen, count, chosen_header))
                if recognized == REQUIRED.len()
                    && *count == REQUIRED.len()
                    && *chosen_header != header =>
            {
                return Err(format!(
                    "The CSV header is ambiguous: it splits into the required columns with both {} and {} as the separator. Save the file with one separator that does not appear inside header names.",
                    delimiter_name(*chosen),
                    delimiter_name(delimiter)
                ));
            }
            Some((_, count, _)) if recognized <= *count => {}
            _ => best = Some((delimiter, recognized, header)),
        }
    }
    Ok(best.map_or(b',', |(delimiter, _, _)| delimiter))
}

fn delimiter_name(delimiter: u8) -> &'static str {
    match delimiter {
        b';' => "semicolon",
        b'\t' => "tab",
        _ => "comma",
    }
}

pub(crate) fn workbook_sheets(path: &Path) -> Result<Vec<String>, String> {
    let book = open_import_workbook(path)?;
    let sheets = book.sheet_names();
    if sheets.is_empty() {
        Err("The workbook has no worksheets.".into())
    } else {
        Ok(sheets)
    }
}

#[cfg(test)]
pub(crate) fn read_sheet(
    path: &Path,
    base: &Form,
    sheet_index: usize,
) -> Result<Vec<BulkJob>, String> {
    read_sheet_with_ack(path, base, sheet_index, false)
}

fn read_sheet_with_ack(
    path: &Path,
    base: &Form,
    sheet_index: usize,
    plaintext_acknowledged: bool,
) -> Result<Vec<BulkJob>, String> {
    let mut book = open_import_workbook(path)?;
    let (header_row_number, rows) = book.read_sheet(sheet_index)?;
    let mut rows = rows.into_iter();
    let headers = normalize_headers(
        rows.next().ok_or("The worksheet is empty.")?,
        crate::MAX_BULK_IMPORT_COLUMNS,
        "worksheet",
    )?;
    let allow_plaintext_secrets =
        plaintext_import_allowed(plaintext_secret_import_enabled(), plaintext_acknowledged);
    validate_headers(&headers, allow_plaintext_secrets)?;
    let defaults = BulkJob::defaults_from_form(base);
    let mut jobs = Vec::new();
    for (index, row) in rows.enumerate() {
        if index >= crate::MAX_BULK_IMPORT_ROWS {
            return Err(format!(
                "The worksheet exceeds the {}-row import limit.",
                crate::MAX_BULK_IMPORT_ROWS
            ));
        }
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        let row_number = header_row_number + index + 1;
        let values = record_values(&headers, row, row_number, crate::MAX_BULK_IMPORT_CELL_BYTES)?;
        jobs.push(job_from_values_with_defaults(
            values,
            base,
            row_number,
            allow_plaintext_secrets,
            Arc::clone(&defaults),
        )?);
    }
    if jobs.is_empty() {
        return Err("The worksheet has no migration rows.".into());
    }
    Ok(jobs)
}

/// Reject oversized XLSX worksheets before the bounded reader constructs the
/// selected sheet's dense cell matrix. The dimension is near the start of the
/// XML part, so this bounded probe does not materialize the workbook entry.
fn validate_xlsx_archive<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Result<(), String> {
    let entry_count = archive.len();
    validate_workbook_container_limits(entry_count, 0)?;
    let mut uncompressed_bytes = 0_u64;
    let mut worksheet_found = false;
    for index in 0..entry_count {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Could not inspect worksheet dimensions: {error}"))?;
        let entry_name = entry
            .name()
            .map_err(|error| format!("Could not inspect worksheet entry name: {error}"))?
            .into_owned();
        uncompressed_bytes = uncompressed_bytes.saturating_add(entry.size());
        validate_workbook_container_limits(entry_count, uncompressed_bytes)?;
        if entry_name.ends_with("sharedStrings.xml") {
            if entry.size() > crate::MAX_BULK_IMPORT_SHARED_STRING_BYTES {
                return Err(format!(
                    "The XLSX shared-string table exceeds the {}-byte limit.",
                    crate::MAX_BULK_IMPORT_SHARED_STRING_BYTES
                ));
            }
            validate_xlsx_shared_strings(&mut entry)?;
        }
        if entry_name.ends_with("styles.xml") && entry.size() > crate::MAX_BULK_IMPORT_STYLES_BYTES
        {
            return Err(format!(
                "The XLSX style table exceeds the {}-byte limit.",
                crate::MAX_BULK_IMPORT_STYLES_BYTES
            ));
        }
        // Worksheet parts are discovered through OPC relationships; their
        // package path is not required to use the conventional
        // `xl/worksheets/` directory. Inspect the XML root rather than
        // trusting the archive filename, so every worksheet receives the same
        // resource and value preflight.
        drop(entry);
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Could not inspect worksheet dimensions: {error}"))?;
        if !is_xml_part(&mut entry)? {
            continue;
        }
        drop(entry);
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Could not inspect worksheet dimensions: {error}"))?;
        let is_worksheet = validate_xlsx_sheet_entry_dimensions(&mut entry)
            .map_err(|error| format!("XLSX part {entry_name:?}: {error}"))?;
        if !is_worksheet {
            continue;
        }
        worksheet_found = true;
        drop(entry);
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Could not inspect worksheet cell types: {error}"))?;
        validate_xlsx_cells(&mut entry)?;
    }
    if !worksheet_found {
        return Err("The XLSX archive contains no worksheets.".into());
    }
    Ok(())
}

/// Quickly identify XML package parts without relying on their file names or
/// extensions. OPC relationships may target worksheet parts at arbitrary
/// paths. A non-XML binary ZIP entry is skipped; unusually long leading
/// whitespace is rejected rather than allowing a worksheet to evade probing.
fn is_xml_part<R: Read>(entry: &mut R) -> Result<bool, String> {
    const MAX_XML_PREAMBLE_BYTES: usize = 128 * 1024;
    let mut input = BufReader::new(entry);
    let mut consumed = 0_usize;
    let mut initial = true;
    loop {
        let available = input
            .fill_buf()
            .map_err(|error| format!("Could not inspect XLSX package part: {error}"))?;
        if available.is_empty() {
            return Ok(false);
        }
        if initial && looks_like_utf16_xml(available) {
            return Ok(true);
        }
        if initial && available.starts_with(b"\xEF\xBB\xBF") {
            input.consume(3);
            consumed += 3;
            initial = false;
            continue;
        }
        initial = false;
        let whitespace = available
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count();
        if whitespace == 0 {
            return Ok(available[0] == b'<');
        }
        consumed = consumed.saturating_add(whitespace);
        if consumed > MAX_XML_PREAMBLE_BYTES {
            return Err("An XLSX XML part has an excessive leading preamble.".into());
        }
        input.consume(whitespace);
    }
}

/// XML processors can identify UTF-16 from its BOM or the byte pattern of the
/// first markup character. Recognize these signatures before the UTF-8
/// whitespace probe so the encoding-aware XML parser validates the same
/// worksheet parts in the package.
fn looks_like_utf16_xml(bytes: &[u8]) -> bool {
    let (little_endian, mut offset) = if bytes.starts_with(b"\xFF\xFE") {
        (true, 2)
    } else if bytes.starts_with(b"\xFE\xFF") {
        (false, 2)
    } else if bytes.len() >= 4 && bytes[1] == 0 {
        (true, 0)
    } else if bytes.len() >= 4 && bytes[0] == 0 {
        (false, 0)
    } else {
        return false;
    };
    while offset + 1 < bytes.len() {
        let code_unit = if little_endian {
            u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
        } else {
            u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
        };
        match code_unit {
            0x20 | 0x09 | 0x0A | 0x0D => offset += 2,
            0x3C => return true,
            _ => return false,
        }
    }
    false
}

/// Validate worksheet cell coordinates and boolean values before the selected
/// sheet is materialized. This applies to every worksheet part, including
/// sheets the operator does not select.
fn validate_xlsx_cells<R: Read>(entry: &mut R) -> Result<(), String> {
    let mut xml = XmlReader::from_reader(DecodingReader::new(BufReader::new(entry)));
    xml.config_mut().trim_text(false);
    let mut buffer = Vec::with_capacity(1024);
    let mut in_boolean_cell = false;
    let mut value_started = false;
    let mut in_value = false;
    let mut value_valid = false;
    let mut value = String::with_capacity(2);

    loop {
        buffer.clear();
        match xml
            .read_event_into(&mut buffer)
            .map_err(|error| format!("The worksheet XML is malformed: {error}"))?
        {
            XmlEvent::Start(element) if element.local_name().as_ref() == "row" => {
                validate_xlsx_row_attributes(&element)?;
            }
            XmlEvent::Start(element) if element.local_name().as_ref() == "c" => {
                if in_boolean_cell {
                    return Err("The worksheet contains nested cells.".into());
                }
                in_boolean_cell = validate_xlsx_cell_attributes(&element)?;
                value_started = false;
                in_value = false;
                value_valid = false;
                value.clear();
            }
            XmlEvent::Empty(element) if element.local_name().as_ref() == "c" => {
                if validate_xlsx_cell_attributes(&element)? {
                    return Err("A boolean-typed worksheet cell has no valid 0/1 value.".into());
                }
            }
            XmlEvent::Start(element) if in_boolean_cell && element.local_name().as_ref() == "v" => {
                if value_started {
                    return Err("A boolean-typed worksheet cell has duplicate values.".into());
                }
                value_started = true;
                in_value = true;
            }
            XmlEvent::Empty(element) if in_boolean_cell && element.local_name().as_ref() == "v" => {
                return Err("A boolean-typed worksheet cell has no valid 0/1 value.".into());
            }
            XmlEvent::Text(text) if in_boolean_cell && in_value => {
                if value.len().saturating_add(text.as_ref().len()) > 1 {
                    return Err(
                        "A boolean-typed worksheet cell must contain exactly 0 or 1.".into(),
                    );
                }
                value.push_str(text.as_ref());
            }
            XmlEvent::CData(text) if in_boolean_cell && in_value => {
                if value.len().saturating_add(text.as_ref().len()) > 1 {
                    return Err(
                        "A boolean-typed worksheet cell must contain exactly 0 or 1.".into(),
                    );
                }
                value.push_str(text.as_ref());
            }
            XmlEvent::End(element) if in_boolean_cell && element.local_name().as_ref() == "v" => {
                in_value = false;
                if value != "0" && value != "1" {
                    return Err(
                        "A boolean-typed worksheet cell must contain exactly 0 or 1.".into(),
                    );
                }
                value_valid = true;
            }
            XmlEvent::End(element) if in_boolean_cell && element.local_name().as_ref() == "c" => {
                if !value_valid {
                    return Err("A boolean-typed worksheet cell has no valid 0/1 value.".into());
                }
                in_boolean_cell = false;
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }
    if in_boolean_cell {
        return Err("A boolean-typed worksheet cell is incomplete.".into());
    }
    Ok(())
}

fn validate_xlsx_cell_attributes(
    element: &quick_xml::events::BytesStart<'_>,
) -> Result<bool, String> {
    let mut cell_type = None;
    let mut has_reference = false;
    for attribute in element.attributes() {
        let attribute = attribute
            .map_err(|error| format!("The worksheet cell has a malformed attribute: {error}"))?;
        match attribute.key.local_name().as_ref() {
            "t" => {
                if cell_type.is_some() {
                    return Err("The worksheet cell has duplicate type attributes.".into());
                }
                cell_type = Some(attribute.value.as_ref() == "b");
            }
            "r" => {
                if has_reference {
                    return Err("The worksheet cell has duplicate reference attributes.".into());
                }
                has_reference = true;
                let reference = attribute.value.as_ref();
                validate_xlsx_cell_reference(reference)?;
            }
            _ => {}
        }
    }
    Ok(cell_type == Some(true))
}

fn validate_xlsx_row_attributes(element: &quick_xml::events::BytesStart<'_>) -> Result<(), String> {
    let mut has_reference = false;
    for attribute in element.attributes() {
        let attribute = attribute
            .map_err(|error| format!("The worksheet row has a malformed attribute: {error}"))?;
        if attribute.key.local_name().as_ref() != "r" {
            continue;
        }
        if has_reference {
            return Err("The worksheet row has duplicate reference attributes.".into());
        }
        has_reference = true;
        let reference = attribute.value.as_ref();
        let row = reference
            .parse::<u32>()
            .ok()
            .filter(|row| *row > 0)
            .ok_or_else(|| "The worksheet row reference is invalid.".to_owned())?;
        if row > crate::MAX_BULK_IMPORT_ROWS as u32 + 1 {
            return Err(format!(
                "The worksheet contains a row beyond the {}-row import limit.",
                crate::MAX_BULK_IMPORT_ROWS
            ));
        }
    }
    Ok(())
}

fn validate_xlsx_cell_reference(reference: &str) -> Result<(), String> {
    let (column, row) = xlsx_reference::parse_cell_reference(reference)?;
    if column > crate::MAX_BULK_IMPORT_COLUMNS {
        return Err(format!(
            "The worksheet contains a cell beyond the {}-column import limit.",
            crate::MAX_BULK_IMPORT_COLUMNS
        ));
    }
    if row > crate::MAX_BULK_IMPORT_ROWS as u32 + 1 {
        return Err(format!(
            "The worksheet contains a cell beyond the {}-row import limit.",
            crate::MAX_BULK_IMPORT_ROWS
        ));
    }
    Ok(())
}

fn validate_xlsx_shared_strings<R: Read>(entry: &mut R) -> Result<(), String> {
    let mut xml = XmlReader::from_reader(DecodingReader::new(BufReader::new(entry)));
    xml.config_mut().trim_text(false);
    let mut buffer = Vec::with_capacity(1024);
    let mut string_count = 0_usize;
    let mut in_string = false;
    let mut string_bytes = 0_usize;
    let mut total_string_bytes = 0_u64;
    loop {
        buffer.clear();
        match xml
            .read_event_into(&mut buffer)
            .map_err(|error| format!("The XLSX shared-string table is malformed: {error}"))?
        {
            XmlEvent::Start(element) if element.local_name().as_ref() == "sst" => {
                let mut declared_count = None;
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|error| {
                        format!("The XLSX shared-string table has a malformed attribute: {error}")
                    })?;
                    if attribute.key.local_name().as_ref() == "uniqueCount" {
                        if declared_count.is_some() {
                            return Err(
                                "The XLSX shared-string table has duplicate uniqueCount attributes."
                                    .into(),
                            );
                        }
                        let count = attribute
                            .value
                            .parse::<usize>()
                            .map_err(|_| "The XLSX shared-string count is invalid.".to_owned())?;
                        declared_count = Some(count);
                    }
                }
                if declared_count.is_some_and(|count| count > crate::MAX_BULK_IMPORT_SHARED_STRINGS)
                {
                    return Err(format!(
                        "The XLSX shared-string table declares more than the {}-string limit.",
                        crate::MAX_BULK_IMPORT_SHARED_STRINGS
                    ));
                }
            }
            XmlEvent::Start(element) if element.local_name().as_ref() == "si" => {
                if in_string {
                    return Err("The XLSX shared-string table has nested entries.".into());
                }
                string_count = string_count.saturating_add(1);
                if string_count > crate::MAX_BULK_IMPORT_SHARED_STRINGS {
                    return Err(format!(
                        "The XLSX shared-string table exceeds the {}-string limit.",
                        crate::MAX_BULK_IMPORT_SHARED_STRINGS
                    ));
                }
                in_string = true;
                string_bytes = 0;
            }
            XmlEvent::Empty(element) if element.local_name().as_ref() == "si" => {
                string_count = string_count.saturating_add(1);
                if string_count > crate::MAX_BULK_IMPORT_SHARED_STRINGS {
                    return Err(format!(
                        "The XLSX shared-string table exceeds the {}-string limit.",
                        crate::MAX_BULK_IMPORT_SHARED_STRINGS
                    ));
                }
            }
            XmlEvent::Text(text) if in_string => {
                let raw_text: &str = text.as_ref();
                let byte_count = raw_text.len();
                string_bytes = string_bytes.saturating_add(byte_count);
                total_string_bytes = total_string_bytes.saturating_add(byte_count as u64);
                if string_bytes > crate::MAX_BULK_IMPORT_CELL_BYTES {
                    return Err(format!(
                        "An XLSX shared string exceeds the {}-byte cell limit.",
                        crate::MAX_BULK_IMPORT_CELL_BYTES
                    ));
                }
                if total_string_bytes > crate::MAX_BULK_IMPORT_SHARED_STRING_BYTES {
                    return Err(format!(
                        "The XLSX shared-string contents exceed the {}-byte limit.",
                        crate::MAX_BULK_IMPORT_SHARED_STRING_BYTES
                    ));
                }
            }
            XmlEvent::CData(text) if in_string => {
                let raw_text: &str = text.as_ref();
                let byte_count = raw_text.len();
                string_bytes = string_bytes.saturating_add(byte_count);
                total_string_bytes = total_string_bytes.saturating_add(byte_count as u64);
                if string_bytes > crate::MAX_BULK_IMPORT_CELL_BYTES {
                    return Err(format!(
                        "An XLSX shared string exceeds the {}-byte cell limit.",
                        crate::MAX_BULK_IMPORT_CELL_BYTES
                    ));
                }
                if total_string_bytes > crate::MAX_BULK_IMPORT_SHARED_STRING_BYTES {
                    return Err(format!(
                        "The XLSX shared-string contents exceed the {}-byte limit.",
                        crate::MAX_BULK_IMPORT_SHARED_STRING_BYTES
                    ));
                }
            }
            XmlEvent::End(element) if element.local_name().as_ref() == "si" => {
                if !in_string {
                    return Err("The XLSX shared-string table has an unmatched entry end.".into());
                }
                in_string = false;
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }
    if in_string {
        return Err("The XLSX shared-string table has an incomplete entry.".into());
    }
    Ok(())
}

fn validate_xlsx_sheet_entry_dimensions<R: Read>(entry: &mut R) -> Result<bool, String> {
    const MAX_DIMENSION_PREFIX_BYTES: usize = 128 * 1024;
    let bounded = entry.take(MAX_DIMENSION_PREFIX_BYTES as u64 + 1);
    let mut xml = XmlReader::from_reader(DecodingReader::new(BufReader::new(bounded)));
    xml.config_mut().trim_text(false);
    let mut buffer = Vec::with_capacity(1024);
    let mut consumed_dimension = None;
    let mut root_seen = false;
    loop {
        buffer.clear();
        let event = xml
            .read_event_into(&mut buffer)
            .map_err(|error| format!("The worksheet XML is malformed: {error}"))?;
        let (element, empty) = match &event {
            XmlEvent::Start(element) => (Some(element), false),
            XmlEvent::Empty(element) => (Some(element), true),
            XmlEvent::Eof => (None, false),
            _ => continue,
        };
        let Some(element) = element else {
            return Err("The worksheet contains no early sheetData element; refusing to materialize an unbounded sheet.".into());
        };
        let local_name = element.local_name();
        if !root_seen {
            root_seen = true;
            if local_name.as_ref() != "worksheet" {
                return Ok(false);
            }
            if empty {
                return Ok(true);
            }
            continue;
        }
        if local_name.as_ref() == "dimension" {
            if consumed_dimension.is_some() {
                return Err("The worksheet contains duplicate dimension elements.".into());
            }
            let mut reference = None;
            for attribute in element.attributes() {
                let attribute = attribute.map_err(|error| {
                    format!("The worksheet dimension has a malformed attribute: {error}")
                })?;
                if attribute.key.local_name().as_ref() == "ref" {
                    if reference.is_some() {
                        return Err("The worksheet dimension has duplicate ref attributes.".into());
                    }
                    reference = Some(attribute.value.into_owned());
                }
            }
            consumed_dimension = Some(
                reference
                    .ok_or_else(|| "The worksheet dimension has no ref attribute.".to_owned())?,
            );
        } else if local_name.as_ref() == "sheetData" {
            if empty {
                if let Some(reference) = consumed_dimension.as_deref() {
                    validate_xlsx_dimension_reference(reference)?;
                }
                return Ok(true);
            }
            let reference = consumed_dimension.as_deref().ok_or_else(|| {
                "The worksheet does not declare a bounded early dimension; refusing to materialize it.".to_owned()
            })?;
            validate_xlsx_dimension_reference(reference)?;
            return Ok(true);
        }
    }
}

fn validate_xlsx_dimension_reference(reference: &str) -> Result<(), String> {
    xlsx_reference::validate_dimension_reference(
        reference,
        crate::MAX_BULK_IMPORT_COLUMNS,
        crate::MAX_BULK_IMPORT_ROWS as u32 + 1,
    )
}

#[cfg(test)]
pub(crate) fn job_from_values(
    values: HashMap<String, String>,
    base: &Form,
    row: usize,
    allow_plaintext_secrets: bool,
) -> Result<BulkJob, String> {
    job_from_values_with_defaults(
        values,
        base,
        row,
        allow_plaintext_secrets,
        BulkJob::defaults_from_form(base),
    )
}

fn job_from_values_with_defaults(
    mut values: HashMap<String, String>,
    base: &Form,
    row: usize,
    allow_plaintext_secrets: bool,
    defaults: Arc<BatchPlanDefaults>,
) -> Result<BulkJob, String> {
    let source_password = values.remove("source_password").unwrap_or_default();
    let destination_password = values.remove("destination_password").unwrap_or_default();
    // Direct callers may provide password fields, but plaintext material is
    // still opt-in. Normal CSV/XLSX imports are rejected earlier when the
    // headers themselves are present unless the operator explicitly enables
    // plaintext-secret imports.
    if !allow_plaintext_secrets
        && (!source_password.trim().is_empty() || !destination_password.trim().is_empty())
    {
        return Err(
            "Plaintext credential values were detected. Use credential IDs instead (source_credential_id, destination_credential_id). To import passwords, set MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1 and confirm the warning for this import in the GUI.".into()
        );
    }
    let get = |key: &str| {
        values
            .get(key)
            .map(String::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let mut form = base.clone_without_credentials();
    form.profile.source_host = get("source_host");
    form.profile.source_user = get("source_user");
    if values.contains_key("source_rate_tenant") {
        form.profile.source_rate_tenant = get("source_rate_tenant");
    }
    if let Some(value) = values.get("source_credential_id") {
        form.profile.source_credential_id = value.trim().to_owned();
    }
    form.source_password = SecretString::new(source_password);
    form.profile.destination_host = get("destination_host");
    form.profile.destination_user = get("destination_user");
    if values.contains_key("destination_rate_tenant") {
        form.profile.destination_rate_tenant = get("destination_rate_tenant");
    }
    if let Some(value) = values.get("destination_credential_id") {
        form.profile.destination_credential_id = value.trim().to_owned();
    }
    if let Some(project_name) = values.get("project_name") {
        let project_name = project_name.trim();
        if !project_name.is_empty() {
            form.profile.name = project_name.chars().take(120).collect();
        }
    }
    form.destination_password = SecretString::new(destination_password);
    form.validate_for_import()
        .map_err(|error| format!("Row {row}: {error}"))?;
    let label = values
        .get("name")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| {
            format!(
                "Row {row}: {} → {}",
                form.profile.source_user, form.profile.destination_user
            )
        });
    Ok(BulkJob::from_form_with_defaults(
        label,
        form,
        "imported".into(),
        defaults,
    ))
}

#[cfg(test)]
pub(crate) fn validate_bulk_import_file(path: &Path) -> Result<(), String> {
    let size = open_import_file(path)
        .map_err(|error| format!("Could not inspect import file: {error}"))?
        .metadata()
        .map_err(|error| format!("Could not inspect import file: {error}"))?
        .len();
    if size > crate::MAX_BULK_IMPORT_BYTES {
        return Err(format!(
            "The import file is {size} bytes; the limit is {} bytes.",
            crate::MAX_BULK_IMPORT_BYTES
        ));
    }
    Ok(())
}

fn open_import_file(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("Could not open import file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect import file: {error}"))?;
    if !metadata.is_file() {
        return Err("The import path must refer to a regular file.".into());
    }
    if metadata.len() > crate::MAX_BULK_IMPORT_BYTES {
        return Err(format!(
            "The import file is {} bytes; the limit is {} bytes.",
            metadata.len(),
            crate::MAX_BULK_IMPORT_BYTES
        ));
    }
    Ok(file)
}

pub(crate) fn validate_workbook_container_limits(
    entry_count: usize,
    uncompressed_bytes: u64,
) -> Result<(), String> {
    if entry_count > crate::MAX_BULK_IMPORT_ARCHIVE_ENTRIES {
        return Err(format!(
            "The XLSX archive contains {entry_count} entries; the limit is {}.",
            crate::MAX_BULK_IMPORT_ARCHIVE_ENTRIES
        ));
    }
    if uncompressed_bytes > crate::MAX_BULK_IMPORT_UNCOMPRESSED_BYTES {
        return Err(format!(
            "The XLSX archive expands to {uncompressed_bytes} bytes; the limit is {} bytes.",
            crate::MAX_BULK_IMPORT_UNCOMPRESSED_BYTES
        ));
    }
    Ok(())
}

fn open_import_workbook(path: &Path) -> Result<XlsxWorkbook, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let file = open_import_file(path)?;
    match extension.as_str() {
        "xlsx" => {
            let archive = zip::ZipArchive::new(file)
                .map_err(|error| format!("The XLSX archive is invalid: {error}"))?;
            XlsxWorkbook::new(archive)
        }
        "xls" => Err("Legacy .xls imports are disabled because MailSwiftSync supports only bounded OOXML XLSX and CSV import formats. Convert the workbook to .xlsx or CSV.".into()),
        _ => Err("Choose a .xlsx workbook.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::xlsx_reference::{
        parse_cell_reference as parse_xlsx_cell_reference, validate_dimension_reference,
    };
    use super::{
        BulkJob, is_xml_part, job_from_values, open_import_workbook, plaintext_import_allowed,
        plaintext_secrets_allowed, validate_xlsx_cells, validate_xlsx_shared_strings,
        validate_xlsx_sheet_entry_dimensions,
    };
    use crate::migration_plan::Form;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::Arc;
    use zip::write::SimpleFileOptions;

    fn write_minimal_xlsx(path: &std::path::Path, worksheet_xml: &str) {
        write_minimal_xlsx_at(path, worksheet_xml, "worksheets/sheet1.xml");
    }

    fn write_minimal_xlsx_at(
        path: &std::path::Path,
        worksheet_xml: &str,
        worksheet_part_path: &str,
    ) {
        write_minimal_xlsx_part(path, worksheet_xml.as_bytes(), worksheet_part_path);
    }

    fn write_minimal_xlsx_part(
        path: &std::path::Path,
        worksheet_xml: &[u8],
        worksheet_part_path: &str,
    ) {
        let file = std::fs::File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        let content_types = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/{worksheet_part_path}" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#
        );
        let workbook_relationship = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="{worksheet_part_path}"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#
        );
        let worksheet_entry = format!("xl/{worksheet_part_path}");
        for (name, contents) in [
            ("[Content_Types].xml", content_types.as_str()),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Mailboxes" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            ("xl/_rels/workbook.xml.rels", workbook_relationship.as_str()),
            (
                "xl/sharedStrings.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>unused fixture value</t></si></sst>"#,
            ),
        ] {
            archive.start_file(name, options).unwrap();
            std::io::Write::write_all(&mut archive, contents.as_bytes()).unwrap();
        }
        archive.start_file(worksheet_entry, options).unwrap();
        std::io::Write::write_all(&mut archive, worksheet_xml).unwrap();
        archive.finish().unwrap();
    }

    #[test]
    fn plaintext_secret_switch_requires_exactly_one() {
        for value in [None, Some(""), Some("0"), Some("false"), Some("no")] {
            assert!(!plaintext_secrets_allowed(value));
        }
        assert!(plaintext_secrets_allowed(Some("1")));
    }

    #[test]
    fn plaintext_import_requires_both_opt_in_and_one_import_acknowledgement() {
        assert!(!plaintext_import_allowed(false, false));
        assert!(!plaintext_import_allowed(false, true));
        assert!(!plaintext_import_allowed(true, false));
        assert!(plaintext_import_allowed(true, true));
    }

    #[test]
    fn bulk_rows_share_defaults_and_hydrate_mailbox_deltas() {
        let mut base = Form::default();
        base.profile.engine = crate::core::Engine::Dovecot;
        base.profile.source_tls = "starttls".into();
        base.profile.source_host = "source.example".into();
        base.profile.destination_host = "destination.example".into();
        base.profile.source_rate_tenant = "workspace-tenant-guid".into();
        let defaults = BulkJob::defaults_from_form(&base);
        let mut first = base.clone_without_credentials();
        first.profile.source_host = "source-a.example".into();
        first.profile.source_user = "a@example.com".into();
        first.profile.destination_host = "destination-a.example".into();
        first.profile.destination_user = "a@new.example".into();
        let first = BulkJob::from_form_with_defaults(
            "a".into(),
            first,
            "imported".into(),
            Arc::clone(&defaults),
        );
        let mut second = base.clone_without_credentials();
        second.profile.source_host = "source-b.example".into();
        second.profile.source_user = "b@example.com".into();
        second.profile.destination_host = "destination-b.example".into();
        second.profile.destination_user = "b@new.example".into();
        let second = BulkJob::from_form_with_defaults(
            "b".into(),
            second,
            "imported".into(),
            Arc::clone(&defaults),
        );

        assert!(Arc::ptr_eq(&first.defaults, &second.defaults));
        assert!(first.defaults.profile.source_host.is_empty());
        assert_eq!(first.form().profile.source_user, "a@example.com");
        assert_eq!(second.form().profile.destination_user, "b@new.example");
        assert_eq!(first.form().profile.source_tls, "starttls");
        assert_eq!(
            first.form().profile.source_rate_tenant,
            "workspace-tenant-guid"
        );
        assert_eq!(
            second.form().profile.source_rate_tenant,
            "workspace-tenant-guid"
        );
    }

    #[test]
    fn xlsx_dimension_parser_enforces_cell_coordinates() {
        assert_eq!(parse_xlsx_cell_reference("BL100001").unwrap(), (64, 100001));
        assert!(parse_xlsx_cell_reference("XFD1048576").is_ok());
        assert!(parse_xlsx_cell_reference("A").is_err());
        assert!(parse_xlsx_cell_reference(&format!("{}1", "X".repeat(256))).is_err());
        validate_dimension_reference("A1:D2", 64, 100_000).unwrap();
        assert!(validate_dimension_reference("D2:A1", 64, 100_000).is_err());
        assert!(validate_dimension_reference("A2:D1", 64, 100_000).is_err());
        assert!(validate_dimension_reference("D1:A2", 64, 100_000).is_err());
    }

    #[test]
    fn xlsx_relationship_paths_are_normalized_and_cannot_escape_package_root() {
        assert_eq!(
            super::xlsx::resolve_part_path("xl", "worksheets/sheet1.xml").unwrap(),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            super::xlsx::resolve_part_path("xl", "../custom/sheet.xml").unwrap(),
            "custom/sheet.xml"
        );
        assert_eq!(
            super::xlsx::resolve_part_path("xl", "/custom/sheet.xml").unwrap(),
            "custom/sheet.xml"
        );
        for target in ["../../../outside.xml", "bad%Q0.xml", "folder\\sheet.xml"] {
            assert!(
                super::xlsx::resolve_part_path("xl", target).is_err(),
                "invalid relationship target {target:?} must be rejected"
            );
        }
    }

    #[test]
    fn xlsx_import_rejects_reversed_dimensions_before_cell_materialization() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-reversed-dimension-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="D2:A1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>source_host</t></is></c></row></sheetData></worksheet>"#,
        );

        let error = open_import_workbook(&path)
            .err()
            .expect("reversed dimensions must fail during bounded XLSX preflight");
        assert!(error.contains("top-left to bottom-right"), "{error}");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_dimension_probe_fails_closed_for_unbounded_nonempty_sheets() {
        assert!(
            validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(
                b"<worksheet><sheetData><row r=\"1\"/></sheetData></worksheet>",
            ))
            .is_err()
        );
        assert!(
            validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(b"<worksheet><sheetData>",))
                .is_err()
        );
        assert!(
            validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(
                b"<worksheet><sheetData/></worksheet>",
            ))
            .is_ok()
        );
        assert!(validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(
            b"<worksheet><!-- <dimension ref='A1'/> --><dimension ref='XFD1'/><sheetData/></worksheet>",
        ))
        .is_err());
        assert!(
            validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(
                b"<worksheet><dimension bogus='A1'/><sheetData/></worksheet>",
            ))
            .is_err()
        );
        validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(
            b"<x:worksheet xmlns:x='urn:test'><x:dimension x:ref='A1:B10'/><x:sheetData><x:row/></x:sheetData></x:worksheet>",
        ))
        .expect("namespace-qualified OOXML elements and attributes use local names");
        assert!(
            validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(
                b"<worksheet><dimension ref='XFD100000:A1'/><sheetData/></worksheet>",
            ))
            .is_err()
        );
    }

    #[test]
    fn xlsx_shared_string_reservations_and_values_are_bounded() {
        let error =
            validate_xlsx_shared_strings(&mut Cursor::new(br#"<sst uniqueCount="1000001"></sst>"#))
                .unwrap_err();
        assert!(error.contains("string limit"));

        let oversized_string = format!(
            "<sst uniqueCount=\"1\"><si><t>{}</t></si></sst>",
            "x".repeat(crate::MAX_BULK_IMPORT_CELL_BYTES + 1)
        );
        let error = validate_xlsx_shared_strings(&mut Cursor::new(oversized_string.as_bytes()))
            .unwrap_err();
        assert!(error.contains("cell limit"));

        validate_xlsx_shared_strings(&mut Cursor::new(
            br#"<sst uniqueCount="0"><si><t>first</t></si><si><t>second</t></si></sst>"#,
        ))
        .expect("actual shared strings are counted even if uniqueCount lies low");
    }

    /// Peak resident memory of this process, in KiB (Linux only).
    fn peak_rss_kib() -> Option<u64> {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()
    }

    #[test]
    #[ignore = "opt-in release import benchmark; run scripts/benchmark-ui-scale.sh"]
    fn import_scale_benchmark() {
        let rows = crate::MAX_BULK_IMPORT_ROWS;
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-scale-{}.csv",
            uuid::Uuid::new_v4()
        ));
        let mut csv = String::from(
            "project_name,name,source_host,source_user,source_credential_id,destination_host,destination_user,destination_credential_id\n",
        );
        for index in 0..rows {
            csv.push_str(&format!(
                "Scale,mailbox {index},imap.source.example,user{index}@source.example,src-{index},imap.destination.example,user{index}@destination.example,dst-{index}\n"
            ));
        }
        std::fs::write(&path, csv).unwrap();
        let rss_before = peak_rss_kib();
        let started = std::time::Instant::now();
        let jobs = super::read_csv(&path, &crate::Form::default());
        let import_ms = started.elapsed().as_millis();
        std::fs::remove_file(&path).unwrap();
        let jobs = jobs.expect("scale CSV must import");
        assert_eq!(jobs.len(), rows);
        // Peak RSS growth while importing; 0 when /proc is unavailable.
        let import_rss_mib = rss_before
            .zip(peak_rss_kib())
            .map_or(0, |(before, after)| after.saturating_sub(before) / 1024);
        eprintln!("scale-import rows={rows} import_ms={import_ms} import_rss_mib={import_rss_mib}");
    }

    /// Excel's "CSV UTF-8" export starts with a byte-order mark; the first
    /// header must still be recognised.
    #[test]
    fn csv_with_utf8_bom_imports() {
        let path =
            std::env::temp_dir().join(format!("mailswiftsync-bom-{}.csv", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "\u{feff}source_host,source_user,destination_host,destination_user\nimap.source.example,a@source.example,imap.destination.example,a@destination.example\n",
        )
        .unwrap();
        let jobs = super::read_csv(&path, &crate::Form::default());
        std::fs::remove_file(&path).unwrap();
        let jobs = jobs.expect("a BOM must not hide the first header");
        assert_eq!(jobs[0].source_host, "imap.source.example");
    }

    /// Excel in German and other European locales saves "CSV" with
    /// semicolons; tab-separated exports are also common.
    #[test]
    fn semicolon_and_tab_separated_csv_imports() {
        for (separator, label) in [(";", "semicolon"), ("\t", "tab")] {
            let path = std::env::temp_dir().join(format!(
                "mailswiftsync-{label}-{}.csv",
                uuid::Uuid::new_v4()
            ));
            let row = |values: [&str; 4]| values.join(separator);
            std::fs::write(
                &path,
                format!(
                    "\u{feff}{}\n{}\n",
                    row([
                        "source_host",
                        "source_user",
                        "destination_host",
                        "destination_user"
                    ]),
                    row([
                        "imap.source.example",
                        "a,b@source.example",
                        "imap.destination.example",
                        "a@destination.example"
                    ]),
                ),
            )
            .unwrap();
            let jobs = super::read_csv(&path, &crate::Form::default());
            std::fs::remove_file(&path).unwrap();
            let jobs = jobs.unwrap_or_else(|error| panic!("{label}: {error}"));
            assert_eq!(jobs[0].source_host, "imap.source.example", "{label}");
        }
        assert_eq!(super::detect_csv_delimiter(b"a,b;c,d\nx;y;z;w"), Ok(b','));
        assert_eq!(super::detect_csv_delimiter(b""), Ok(b','));
    }

    /// A separator inside a quoted header field is part of that field, not a
    /// column boundary, so it must not outvote the real separator.
    #[test]
    fn delimiter_detection_ignores_separators_inside_quoted_headers() {
        for (header, expected) in [
            (
                &b"\"label, previous, legacy, old, retired, archived\";source_host;source_user;destination_host;destination_user\n"[..],
                b';',
            ),
            (
                &b"\"label; notes; owner\",source_host,source_user,destination_host,destination_user\n"[..],
                b',',
            ),
            (
                &b"\"say \"\"a,b,c\"\"\"\tsource_host\tsource_user\tdestination_host\tdestination_user\n"[..],
                b'\t',
            ),
            (
                "\u{feff}\"a,b,c\";source_host;source_user;destination_host;destination_user\n"
                    .as_bytes(),
                b';',
            ),
        ] {
            assert_eq!(
                super::detect_csv_delimiter(header),
                Ok(expected),
                "{}",
                String::from_utf8_lossy(header)
            );
        }
    }

    #[test]
    fn quoted_comma_header_in_semicolon_csv_imports() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-quoted-header-{}.csv",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            "\u{feff}\"label, previous, legacy, old, retired, archived\";source_host;source_user;destination_host;destination_user\n\"Team \"\"A\"\", West\";imap.source.example;a@source.example;imap.destination.example;a@destination.example\n",
        )
        .unwrap();
        let jobs = super::read_csv(&path, &crate::Form::default());
        std::fs::remove_file(&path).unwrap();
        let jobs = jobs.expect("a quoted comma must not choose the comma separator");
        assert_eq!(jobs[0].source_host, "imap.source.example");
        assert_eq!(jobs[0].destination_user, "a@destination.example");
    }

    #[test]
    fn legacy_xls_import_fails_closed() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-legacy-xls-header-{}-{}.xls",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"legacy xls is disabled").unwrap();

        let error = open_import_workbook(&path)
            .err()
            .expect("legacy XLS must fail closed");
        assert!(error.contains("Legacy .xls imports are disabled"));

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_validation_and_bounded_reader_use_the_same_open_file() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-snapshot-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>source_host</t></is></c></row></sheetData></worksheet>"#,
        );
        assert_eq!(super::workbook_sheets(&path).unwrap(), vec!["Mailboxes"]);
        let mut workbook = super::open_import_workbook(&path).unwrap();
        let (row, worksheet) = workbook.read_sheet(0).unwrap();
        assert_eq!(row, 1);
        assert_eq!(worksheet[0][0], "source_host");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_reader_handles_shared_inline_numeric_boolean_and_sparse_cells() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-values-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A2:D3"/><sheetData><row r="2"><c r="A2" t="s"><v>0</v></c><c r="B2" t="inlineStr"><is><r><t>A &amp; </t></r><r><t>B</t></r></is></c><c r="D2"><v>42.5</v></c></row><row r="3"><c r="A3" t="b"><v>1</v></c></row></sheetData></worksheet>"#,
        );
        let mut workbook = open_import_workbook(&path).unwrap();
        let (start_row, rows) = workbook.read_sheet(0).unwrap();
        assert_eq!(start_row, 2);
        assert_eq!(rows[0], ["unused fixture value", "A & B", "", "42.5"]);
        assert_eq!(rows[1], ["TRUE", "", "", ""]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_reader_rejects_duplicate_values_and_cells_outside_rows() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-malformed-worksheet-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        for worksheet in [
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1"><v>1</v><v>2</v></c></row></sheetData></worksheet>"#,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><c r="A1"><v>1</v></c><sheetData/></worksheet>"#,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A2"><v>1</v></c></row></sheetData></worksheet>"#,
        ] {
            write_minimal_xlsx(&path, worksheet);
            let mut workbook = open_import_workbook(&path).unwrap();
            assert!(
                workbook.read_sheet(0).is_err(),
                "malformed worksheet should be rejected: {worksheet}"
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_reader_builds_import_jobs_from_the_selected_sheet() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-jobs-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A2:D3"/><sheetData><row r="2"><c r="A2" t="inlineStr"><is><t>source_host</t></is></c><c r="B2" t="inlineStr"><is><t>source_user</t></is></c><c r="C2" t="inlineStr"><is><t>destination_host</t></is></c><c r="D2" t="inlineStr"><is><t>destination_user</t></is></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>imap.old.example</t></is></c><c r="B3" t="inlineStr"><is><t>user@old.example</t></is></c><c r="C3" t="inlineStr"><is><t>imap.new.example</t></is></c><c r="D3" t="inlineStr"><is><t>user@new.example</t></is></c></row></sheetData></worksheet>"#,
        );
        let jobs = super::read_sheet(&path, &Form::default(), 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].source_host, "imap.old.example");
        assert_eq!(jobs[0].source_user, "user@old.example");
        assert_eq!(jobs[0].destination_host, "imap.new.example");
        assert_eq!(jobs[0].destination_user, "user@new.example");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_boolean_cell_values_are_strictly_validated() {
        for value in ["0", "1"] {
            let xml = format!(
                "<worksheet><sheetData><row><c t=\"b\"><v>{value}</v></c></row></sheetData></worksheet>"
            );
            validate_xlsx_cells(&mut Cursor::new(xml))
                .expect("OOXML boolean values 0 and 1 are valid");
        }
        for value in [
            "", "false", "TRUE", "00", "0.0", "-1", "2", "banana", "#REF!",
        ] {
            let xml = format!(
                "<worksheet><sheetData><row><c t=\"b\"><v>{value}</v></c></row></sheetData></worksheet>"
            );
            assert!(
                validate_xlsx_cells(&mut Cursor::new(xml)).is_err(),
                "invalid boolean-typed cell value {value:?} must be rejected"
            );
        }
    }

    #[test]
    fn xlsx_cell_and_row_references_are_bounded() {
        for xml in [
            r#"<worksheet><sheetData><row r="99999999999"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#,
            r#"<worksheet><sheetData><row r="100002"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#,
            r#"<worksheet><sheetData><row r="1"><c r="A99999999999"><v>1</v></c></row></sheetData></worksheet>"#,
            r#"<worksheet><sheetData><row r="1"><c r="ZZZZZZZ1"><v>1</v></c></row></sheetData></worksheet>"#,
        ] {
            assert!(
                validate_xlsx_cells(&mut Cursor::new(xml)).is_err(),
                "out-of-range references must be rejected by XLSX preflight: {xml}"
            );
        }
    }

    #[test]
    fn xlsx_import_rejects_overflowing_cell_and_row_references() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-overflowing-reference-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        for worksheet in [
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="99999999999"><c r="A1" t="inlineStr"><is><t>source_host</t></is></c></row></sheetData></worksheet>"#,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A99999999999" t="inlineStr"><is><t>source_host</t></is></c></row></sheetData></worksheet>"#,
        ] {
            write_minimal_xlsx(&path, worksheet);
            let error = open_import_workbook(&path)
                .err()
                .expect("overflowing references must fail in bounded XLSX preflight");
            assert!(
                error.contains("row") || error.contains("limit"),
                "unexpected preflight error: {error}"
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_import_rejects_malformed_boolean_cells() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-malformed-boolean-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="b"><v>banana</v></c></row></sheetData></worksheet>"#,
        );
        let error = open_import_workbook(&path)
            .err()
            .expect("invalid boolean cell must fail during raw XLSX preflight");
        assert!(
            error.contains("exactly 0 or 1"),
            "unexpected preflight error: {error}"
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_boolean_preflight_follows_nonstandard_worksheet_part_paths() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-nonstandard-sheet-path-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx_at(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="b"><v>banana</v></c></row></sheetData></worksheet>"#,
            "custom/migration-data.sheet",
        );
        let error = open_import_workbook(&path)
            .err()
            .expect("malformed boolean must be rejected regardless of worksheet part path");
        assert!(
            error.contains("exactly 0 or 1"),
            "unexpected preflight error: {error}"
        );

        write_minimal_xlsx_at(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="b"><v>1</v></c></row></sheetData></worksheet>"#,
            "custom/migration-data.sheet",
        );
        let mut workbook = open_import_workbook(&path)
            .expect("a valid relationship-targeted worksheet part must remain importable");
        let (_, worksheet) = workbook.read_sheet(0).unwrap();
        assert_eq!(worksheet[0][0], "TRUE");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_boolean_preflight_handles_utf16_worksheet_parts() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-utf16-sheet-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        let xml = r#"<?xml version="1.0" encoding="UTF-16"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="b"><v>banana</v></c></row></sheetData></worksheet>"#;
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        assert!(is_xml_part(&mut Cursor::new(bytes.clone())).unwrap());
        assert!(validate_xlsx_sheet_entry_dimensions(&mut Cursor::new(bytes.clone())).unwrap());
        write_minimal_xlsx_part(&path, &bytes, "custom/migration-data.sheet");

        let error = open_import_workbook(&path)
            .err()
            .expect("malformed UTF-16 boolean must be rejected by preflight");
        assert!(
            error.contains("exactly 0 or 1"),
            "unexpected error: {error}"
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_actual_cell_coordinates_are_bounded_before_range_materialization() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-coordinate-limit-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="XFD1" t="inlineStr"><is><t>far</t></is></c></row></sheetData></worksheet>"#,
        );
        let error = super::read_sheet(&path, &Form::default(), 0)
            .err()
            .expect("a cell outside the column limit must fail before range creation");
        assert!(error.contains("column"));
        std::fs::remove_file(&path).unwrap();

        write_minimal_xlsx(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:BL100001"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>first</t></is></c></row><row r="100001"><c r="BL100001" t="inlineStr"><is><t>last</t></is></c></row></sheetData></worksheet>"#,
        );
        let error = super::read_sheet(&path, &Form::default(), 0)
            .err()
            .expect("a sparse range that expands past the area budget must be rejected");
        assert!(error.contains("cell import limit"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn project_name_import_metadata_is_separate_from_mailbox_label() {
        let values = HashMap::from([
            ("project_name".into(), "Acme Corp cutover".into()),
            ("name".into(), "finance mailbox".into()),
            ("source_host".into(), "old.example.test".into()),
            ("source_user".into(), "finance@old.example.test".into()),
            ("destination_host".into(), "new.example.test".into()),
            ("destination_user".into(), "finance@new.example.test".into()),
        ]);

        let job = job_from_values(values, &Form::default(), 2, false).unwrap();
        assert_eq!(job.form().profile.name, "Acme Corp cutover");
        assert_eq!(job.label, "finance mailbox");
    }

    #[test]
    fn tenant_rate_scopes_import_as_row_deltas() {
        let values = HashMap::from([
            ("source_host".into(), "old.example.test".into()),
            ("source_user".into(), "finance@brand-a.example".into()),
            ("source_rate_tenant".into(), "Workspace-Tenant".into()),
            ("destination_host".into(), "new.example.test".into()),
            ("destination_user".into(), "finance@brand-b.example".into()),
            ("destination_rate_tenant".into(), "Exchange-Tenant".into()),
        ]);
        let job = job_from_values(values, &Form::default(), 2, false).unwrap();
        assert_eq!(job.form().profile.source_rate_tenant, "Workspace-Tenant");
        assert_eq!(
            job.form().profile.destination_rate_tenant,
            "Exchange-Tenant"
        );
        assert!(job.defaults.profile.source_rate_tenant.is_empty());
        assert!(job.defaults.profile.destination_rate_tenant.is_empty());
    }

    #[test]
    fn imported_rows_do_not_copy_base_credentials() {
        let base = Form {
            source_password: "source-secret".into(),
            destination_password: "destination-secret".into(),
            ..Form::default()
        };
        let values = HashMap::from([
            ("source_host".into(), "old.example.test".into()),
            ("source_user".into(), "alice@old.example.test".into()),
            ("destination_host".into(), "new.example.test".into()),
            ("destination_user".into(), "alice@new.example.test".into()),
        ]);

        let job = job_from_values(values, &base, 2, false).unwrap();

        assert!(job.form().source_password.is_empty());
        assert!(job.form().destination_password.is_empty());
    }

    #[test]
    #[ignore = "opt-in release scale benchmark; run scripts/benchmark-import-scale.sh"]
    fn scale_import_benchmark() {
        use std::fmt::Write as _;
        use std::time::Instant;

        fn resident_set_bytes() -> Option<u64> {
            let status = std::fs::read_to_string("/proc/self/status").ok()?;
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|kilobytes| kilobytes.saturating_mul(1024))
        }

        fn csv_fixture(rows: usize) -> String {
            let mut csv =
                String::from("source_host,source_user,destination_host,destination_user\n");
            for row in 0..rows {
                writeln!(
                    csv,
                    "source.example,user{row}@source.example,destination.example,user{row}@destination.example"
                )
                .unwrap();
            }
            csv
        }

        fn xlsx_fixture(rows: usize) -> String {
            let mut worksheet = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:D{}"/><sheetData>"#,
                rows + 1
            );
            for row in 1..=rows + 1 {
                let values = if row == 1 {
                    [
                        "source_host".to_owned(),
                        "source_user".to_owned(),
                        "destination_host".to_owned(),
                        "destination_user".to_owned(),
                    ]
                } else {
                    let index = row - 2;
                    [
                        "source.example".to_owned(),
                        format!("user{index}@source.example"),
                        "destination.example".to_owned(),
                        format!("user{index}@destination.example"),
                    ]
                };
                write!(worksheet, "<row r=\"{row}\">").unwrap();
                for (column, value) in ['A', 'B', 'C', 'D'].into_iter().zip(values) {
                    write!(
                        worksheet,
                        "<c r=\"{column}{row}\" t=\"inlineStr\"><is><t>{value}</t></is></c>"
                    )
                    .unwrap();
                }
                worksheet.push_str("</row>");
            }
            worksheet.push_str("</sheetData></worksheet>");
            worksheet
        }

        let base = Form::default();
        for rows in [1_000, 10_000, 100_000] {
            let path = std::env::temp_dir().join(format!(
                "mailswiftsync-scale-csv-{}-{rows}.csv",
                uuid::Uuid::new_v4()
            ));
            let contents = csv_fixture(rows);
            std::fs::write(&path, &contents).unwrap();
            let bytes = std::fs::metadata(&path).unwrap().len();
            let before = resident_set_bytes();
            let started = Instant::now();
            let jobs = super::read_csv(&path, &base).unwrap();
            let elapsed_ms = started.elapsed().as_millis();
            let after = resident_set_bytes();
            assert_eq!(jobs.len(), rows);
            eprintln!(
                "scale-import format=csv rows={rows} bytes={bytes} elapsed_ms={elapsed_ms} rss_before={before:?} rss_after={after:?}"
            );
            std::fs::remove_file(path).unwrap();
        }
        for rows in [10_000, 100_000] {
            let path = std::env::temp_dir().join(format!(
                "mailswiftsync-scale-xlsx-{}-{rows}.xlsx",
                uuid::Uuid::new_v4()
            ));
            let worksheet = xlsx_fixture(rows);
            write_minimal_xlsx(&path, &worksheet);
            let bytes = std::fs::metadata(&path).unwrap().len();
            let before = resident_set_bytes();
            let started = Instant::now();
            let jobs = super::read_sheet(&path, &base, 0).unwrap();
            let elapsed_ms = started.elapsed().as_millis();
            let after = resident_set_bytes();
            assert_eq!(jobs.len(), rows);
            eprintln!(
                "scale-import format=xlsx rows={rows} bytes={bytes} elapsed_ms={elapsed_ms} rss_before={before:?} rss_after={after:?}"
            );
            std::fs::remove_file(path).unwrap();
        }
    }
}
