//! Untrusted mailbox-list import and structural validation.
//!
//! Parsing is deliberately independent from egui state. This module owns the
//! worker thread, file limits, worksheet selection, row validation, and
//! conversion into import jobs; the UI owns only queue presentation and
//! result application.

use crate::{Form, SecretString};
use calamine::{Reader, Sheets, Xlsx, open_workbook_from_rs};
use quick_xml::Reader as XmlReader;
use quick_xml::events::Event as XmlEvent;
use std::collections::HashMap;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};
use std::thread;

mod rows;
mod xlsx_reference;
pub(crate) use rows::validate_headers;
use rows::{normalize_headers, record_values};

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
    let mut reader = csv::ReaderBuilder::new()
        .from_reader(file.take(crate::MAX_BULK_IMPORT_BYTES.saturating_add(1)));
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
    if reader.into_inner().limit() == 0 {
        return Err(format!(
            "The import file exceeds the {}-byte limit.",
            crate::MAX_BULK_IMPORT_BYTES
        ));
    }
    Ok(jobs)
}

pub(crate) fn workbook_sheets(path: &Path) -> Result<Vec<String>, String> {
    let book = open_import_workbook(path)?;
    let sheets = book.sheet_names().to_vec();
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
    validate_xlsx_sheet_layout(&mut book, sheet_index)?;
    let range = book
        .worksheet_range_at(sheet_index)
        .ok_or_else(|| format!("The workbook has no worksheet at index {sheet_index}."))?
        .map_err(|error| error.to_string())?;
    // Calamine ranges start at the first used cell, so report worksheet row
    // numbers relative to that row rather than assuming headers are on row 1.
    let header_row_number = range.start().map_or(1, |(row, _)| row as usize + 1);
    let mut rows = range.rows();
    let headers = normalize_headers(
        rows.next()
            .ok_or("The worksheet is empty.")?
            .iter()
            .map(ToString::to_string),
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
        if row.iter().all(|cell| cell.to_string().trim().is_empty()) {
            continue;
        }
        let row_number = header_row_number + index + 1;
        let values = record_values(
            &headers,
            row.iter().map(|value| value.to_string()),
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
        return Err("The worksheet has no migration rows.".into());
    }
    Ok(jobs)
}

fn validate_xlsx_sheet_layout(
    book: &mut Sheets<BufReader<std::fs::File>>,
    sheet_index: usize,
) -> Result<(), String> {
    let Sheets::Xlsx(book) = book else {
        return Ok(());
    };
    let sheet_name = book
        .sheet_names()
        .get(sheet_index)
        .cloned()
        .ok_or_else(|| format!("The workbook has no worksheet at index {sheet_index}."))?;
    let mut cells = book
        .worksheet_cells_reader(&sheet_name)
        .map_err(|error| format!("Could not inspect worksheet cells: {error}"))?;
    let mut cell_count = 0_usize;
    let mut min_row = u32::MAX;
    let mut min_column = u32::MAX;
    let mut max_row = 0_u32;
    let mut max_column = 0_u32;
    while let Some(cell) = cells
        .next_cell()
        .map_err(|error| format!("Could not inspect worksheet cells: {error}"))?
    {
        let (row, column) = cell.get_position();
        if row > crate::MAX_BULK_IMPORT_ROWS as u32 {
            return Err(format!(
                "The worksheet contains a cell beyond the {}-row import limit.",
                crate::MAX_BULK_IMPORT_ROWS
            ));
        }
        if column > crate::MAX_BULK_IMPORT_COLUMNS as u32 {
            return Err(format!(
                "The worksheet contains a cell beyond the {}-column import limit.",
                crate::MAX_BULK_IMPORT_COLUMNS
            ));
        }
        cell_count = cell_count.saturating_add(1);
        if cell_count > crate::MAX_BULK_IMPORT_WORKSHEET_CELLS {
            return Err(format!(
                "The worksheet exceeds the {}-cell import limit.",
                crate::MAX_BULK_IMPORT_WORKSHEET_CELLS
            ));
        }
        min_row = min_row.min(row);
        min_column = min_column.min(column);
        max_row = max_row.max(row);
        max_column = max_column.max(column);
    }
    if cell_count > 0 {
        let rows = (max_row - min_row + 1) as usize;
        let columns = (max_column - min_column + 1) as usize;
        let area = rows.checked_mul(columns).ok_or_else(|| {
            "The worksheet's used cell range exceeds the supported import size.".to_owned()
        })?;
        if area > crate::MAX_BULK_IMPORT_WORKSHEET_CELLS {
            return Err(format!(
                "The worksheet's used cell range exceeds the {}-cell import limit.",
                crate::MAX_BULK_IMPORT_WORKSHEET_CELLS
            ));
        }
    }
    Ok(())
}

/// Reject obviously oversized XLSX worksheets before Calamine expands the
/// sheet into a cell matrix. The worksheet dimension is near the start of the
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
        let entry_name = entry.name().to_owned();
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
        if !entry_name.starts_with("xl/worksheets/") || !entry_name.ends_with(".xml") {
            continue;
        }
        worksheet_found = true;
        validate_xlsx_sheet_entry_dimensions(&mut entry)?;
    }
    if !worksheet_found {
        return Err("The XLSX archive contains no worksheets.".into());
    }
    Ok(())
}

fn validate_xlsx_shared_strings<R: Read>(entry: &mut R) -> Result<(), String> {
    let mut xml = XmlReader::from_reader(BufReader::new(entry));
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

fn validate_xlsx_sheet_entry_dimensions<R: Read>(entry: &mut R) -> Result<(), String> {
    const MAX_DIMENSION_PREFIX_BYTES: usize = 128 * 1024;
    let mut bounded = entry.take(MAX_DIMENSION_PREFIX_BYTES as u64 + 1);
    let mut xml = XmlReader::from_reader(BufReader::new(&mut bounded));
    xml.config_mut().trim_text(false);
    let mut buffer = Vec::with_capacity(1024);
    let mut consumed_dimension = None;
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
                return Ok(());
            }
            let reference = consumed_dimension.as_deref().ok_or_else(|| {
                "The worksheet does not declare a bounded early dimension; refusing to materialize it.".to_owned()
            })?;
            validate_xlsx_dimension_reference(reference)?;
            return Ok(());
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

fn open_import_workbook(path: &Path) -> Result<Sheets<BufReader<std::fs::File>>, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let file = open_import_file(path)?;
    match extension.as_str() {
        "xlsx" => {
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|error| format!("The XLSX archive is invalid: {error}"))?;
            validate_xlsx_archive(&mut archive)?;
            let mut file = archive.into_inner();
            if file
                .metadata()
                .map_err(|error| format!("Could not inspect XLSX import file: {error}"))?
                .len()
                > crate::MAX_BULK_IMPORT_BYTES
            {
                return Err(format!(
                    "The XLSX import file exceeds the {}-byte limit.",
                    crate::MAX_BULK_IMPORT_BYTES
                ));
            }
            file.seek(SeekFrom::Start(0))
                .map_err(|error| format!("Could not rewind XLSX import: {error}"))?;
            open_workbook_from_rs::<Xlsx<_>, _>(BufReader::new(file))
                .map(Sheets::Xlsx)
                .map_err(|error| format!("The XLSX workbook could not be parsed: {error}"))
        }
        "xls" => Err("Legacy .xls imports are disabled because their parser materializes worksheet ranges before MailSwiftSync can enforce memory bounds. Convert the workbook to .xlsx or CSV.".into()),
        _ => Err("Choose a .xlsx workbook.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::xlsx_reference::parse_cell_reference as parse_xlsx_cell_reference;
    use super::{
        BulkJob, job_from_values, open_import_workbook, plaintext_import_allowed,
        plaintext_secrets_allowed, validate_xlsx_shared_strings,
        validate_xlsx_sheet_entry_dimensions,
    };
    use crate::migration_plan::Form;
    use calamine::{DataType, Reader};
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::Arc;
    use zip::write::SimpleFileOptions;

    fn write_minimal_xlsx(path: &std::path::Path, worksheet_xml: &str) {
        let file = std::fs::File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, contents) in [
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Mailboxes" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            ("xl/worksheets/sheet1.xml", worksheet_xml),
        ] {
            archive.start_file(name, options).unwrap();
            std::io::Write::write_all(&mut archive, contents.as_bytes()).unwrap();
        }
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
    fn xlsx_shared_string_reservations_and_values_are_bounded_before_calamine() {
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

    #[test]
    fn legacy_xls_import_fails_closed_before_calamine_parsing() {
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
    fn xlsx_validation_and_calamine_reads_the_same_open_file() {
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
        let worksheet = workbook.worksheet_range_at(0).unwrap().unwrap();
        assert_eq!(
            worksheet.get((0, 0)).and_then(|cell| cell.get_string()),
            Some("source_host")
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
