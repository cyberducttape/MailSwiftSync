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
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

fn plaintext_secrets_allowed(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

fn allow_plaintext_secrets() -> bool {
    plaintext_secrets_allowed(
        std::env::var("MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS")
            .ok()
            .as_deref(),
    )
}

#[derive(Clone)]
pub(crate) struct BulkJob {
    pub(crate) label: String,
    pub(crate) form: Form,
    pub(crate) state: String,
}

pub(crate) struct PendingSheetImport {
    #[allow(dead_code)]
    pub(crate) path: PathBuf,
    #[allow(dead_code)]
    pub(crate) sheets: Vec<String>,
}

pub(crate) enum BulkImportResult {
    #[allow(dead_code)]
    Jobs(Vec<BulkJob>),
    #[allow(dead_code)]
    Workbook { path: PathBuf, sheets: Vec<String> },
}

/// Start a bounded/validated mailbox-file import away from the egui thread.
/// The caller receives only the future result; file-format dispatch and
/// parser ownership remain with the import domain.
#[allow(dead_code)]
pub(crate) fn spawn_import(
    path: PathBuf,
    base: Form,
) -> Receiver<Result<BulkImportResult, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let result = if ext == "csv" {
            read_csv(&path, &base).map(BulkImportResult::Jobs)
        } else if ext == "xlsx" {
            workbook_sheets(&path).map(|sheets| BulkImportResult::Workbook { path, sheets })
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
) -> Receiver<Result<BulkImportResult, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = read_sheet(&path, &base, sheet_index)
            .map(BulkImportResult::Jobs)
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
    receiver
}

pub(crate) fn read_csv(path: &Path, base: &Form) -> Result<Vec<BulkJob>, String> {
    let file = open_import_file(path)?;
    let mut reader = csv::ReaderBuilder::new()
        .from_reader(file.take(crate::MAX_BULK_IMPORT_BYTES.saturating_add(1)));
    let headers = reader
        .headers()
        .map_err(|error| error.to_string())?
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let allow_plaintext_secrets = allow_plaintext_secrets();
    validate_headers(&headers, allow_plaintext_secrets)?;
    if headers.len() > crate::MAX_BULK_IMPORT_COLUMNS {
        return Err(format!(
            "The file has too many columns; the limit is {}.",
            crate::MAX_BULK_IMPORT_COLUMNS
        ));
    }
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
        let values = record_values(&headers, record.iter(), row_number)?;
        jobs.push(job_from_values(
            values,
            base,
            row_number,
            allow_plaintext_secrets,
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

pub(crate) fn read_sheet(
    path: &Path,
    base: &Form,
    sheet_index: usize,
) -> Result<Vec<BulkJob>, String> {
    let mut book = open_import_workbook(path)?;
    validate_xlsx_sheet_layout(&mut book, sheet_index)?;
    let range = book
        .worksheet_range_at(sheet_index)
        .ok_or_else(|| format!("The workbook has no worksheet at index {sheet_index}."))?
        .map_err(|error| error.to_string())?;
    let mut rows = range.rows();
    let headers = rows
        .next()
        .ok_or("The worksheet is empty.")?
        .iter()
        .map(|value| value.to_string().trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    if headers.len() > crate::MAX_BULK_IMPORT_COLUMNS {
        return Err(format!(
            "The worksheet has too many columns; the limit is {}.",
            crate::MAX_BULK_IMPORT_COLUMNS
        ));
    }
    let allow_plaintext_secrets = allow_plaintext_secrets();
    validate_headers(&headers, allow_plaintext_secrets)?;
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
        let row_number = index + 2;
        let values = record_values(
            &headers,
            row.iter().map(|value| value.to_string()),
            row_number,
        )?;
        jobs.push(job_from_values(
            values,
            base,
            row_number,
            allow_plaintext_secrets,
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
            XmlEvent::Start(element) if element.local_name().as_ref() == b"sst" => {
                let mut declared_count = None;
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|error| {
                        format!("The XLSX shared-string table has a malformed attribute: {error}")
                    })?;
                    if attribute.key.local_name().as_ref() == b"uniqueCount" {
                        if declared_count.is_some() {
                            return Err(
                                "The XLSX shared-string table has duplicate uniqueCount attributes."
                                    .into(),
                            );
                        }
                        let count = std::str::from_utf8(attribute.value.as_ref())
                            .ok()
                            .and_then(|value| value.parse::<usize>().ok())
                            .ok_or_else(|| "The XLSX shared-string count is invalid.".to_owned())?;
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
            XmlEvent::Start(element) if element.local_name().as_ref() == b"si" => {
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
            XmlEvent::Empty(element) if element.local_name().as_ref() == b"si" => {
                string_count = string_count.saturating_add(1);
                if string_count > crate::MAX_BULK_IMPORT_SHARED_STRINGS {
                    return Err(format!(
                        "The XLSX shared-string table exceeds the {}-string limit.",
                        crate::MAX_BULK_IMPORT_SHARED_STRINGS
                    ));
                }
            }
            XmlEvent::Text(text) if in_string => {
                let raw_text: &[u8] = text.as_ref();
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
                let raw_text: &[u8] = text.as_ref();
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
            XmlEvent::End(element) if element.local_name().as_ref() == b"si" => {
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
        if local_name.as_ref() == b"dimension" {
            if consumed_dimension.is_some() {
                return Err("The worksheet contains duplicate dimension elements.".into());
            }
            let mut reference = None;
            for attribute in element.attributes() {
                let attribute = attribute.map_err(|error| {
                    format!("The worksheet dimension has a malformed attribute: {error}")
                })?;
                if attribute.key.local_name().as_ref() == b"ref" {
                    if reference.is_some() {
                        return Err("The worksheet dimension has duplicate ref attributes.".into());
                    }
                    reference = Some(
                        std::str::from_utf8(attribute.value.as_ref())
                            .map_err(|_| {
                                "The worksheet dimension reference is not UTF-8.".to_owned()
                            })?
                            .to_owned(),
                    );
                }
            }
            consumed_dimension = Some(
                reference
                    .ok_or_else(|| "The worksheet dimension has no ref attribute.".to_owned())?,
            );
        } else if local_name.as_ref() == b"sheetData" {
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
    let mut endpoints = reference.split(':');
    let (start_columns, start_rows) = parse_xlsx_cell_reference(
        endpoints
            .next()
            .ok_or_else(|| "The worksheet dimension has no starting cell.".to_owned())?,
    )?;
    let (columns, rows) = if let Some(end) = endpoints.next() {
        if endpoints.next().is_some() {
            return Err("The worksheet dimension contains too many range endpoints.".into());
        }
        let (end_columns, end_rows) = parse_xlsx_cell_reference(end)?;
        (start_columns.max(end_columns), start_rows.max(end_rows))
    } else {
        (start_columns, start_rows)
    };
    if columns > crate::MAX_BULK_IMPORT_COLUMNS {
        return Err(format!(
            "The worksheet declares {columns} columns; the limit is {}.",
            crate::MAX_BULK_IMPORT_COLUMNS
        ));
    }
    if rows > crate::MAX_BULK_IMPORT_ROWS as u32 + 1 {
        return Err(format!(
            "The worksheet declares {rows} rows; the limit is {}.",
            crate::MAX_BULK_IMPORT_ROWS
        ));
    }
    Ok(())
}

fn parse_xlsx_cell_reference(reference: &str) -> Result<(usize, u32), String> {
    let split = reference
        .find(|character: char| character.is_ascii_digit())
        .ok_or_else(|| "The worksheet dimension has no row number.".to_owned())?;
    let (letters, digits) = reference.split_at(split);
    let mut columns = 0_usize;
    for character in letters.chars() {
        if !character.is_ascii_alphabetic() {
            return Err("The worksheet dimension has an invalid column.".into());
        }
        let value = character.to_ascii_uppercase() as usize - 'A' as usize + 1;
        columns = columns
            .checked_mul(26)
            .and_then(|columns| columns.checked_add(value))
            .ok_or_else(|| "The worksheet dimension column is too large.".to_owned())?;
    }
    let rows = digits
        .parse::<u32>()
        .map_err(|_| "The worksheet dimension has an invalid row.".to_owned())?;
    if rows == 0 || columns == 0 {
        return Err("The worksheet dimension cell coordinates must be positive.".into());
    }
    Ok((columns, rows))
}

fn record_values<I>(
    headers: &[String],
    values: I,
    row: usize,
) -> Result<HashMap<String, String>, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let values = values.into_iter().map(Into::into).collect::<Vec<String>>();
    if values.len() != headers.len() {
        return Err(format!(
            "Row {row} has {} values but the header has {} columns.",
            values.len(),
            headers.len()
        ));
    }
    headers
        .iter()
        .zip(values)
        .map(|(header, value)| {
            if value.len() > crate::MAX_BULK_IMPORT_CELL_BYTES {
                return Err(format!(
                    "Row {row} contains a cell larger than {} bytes.",
                    crate::MAX_BULK_IMPORT_CELL_BYTES
                ));
            }
            Ok((header.clone(), value))
        })
        .collect()
}

pub(crate) fn job_from_values(
    mut values: HashMap<String, String>,
    base: &Form,
    row: usize,
    allow_plaintext_secrets: bool,
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
            "Plaintext credential values were detected. Use credential IDs instead (source_credential_id, destination_credential_id), or set MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1 to import password material.".into()
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
    if let Some(value) = values.get("source_credential_id") {
        form.profile.source_credential_id = value.trim().to_owned();
    }
    form.source_password = SecretString::new(source_password);
    form.profile.destination_host = get("destination_host");
    form.profile.destination_user = get("destination_user");
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
    Ok(BulkJob {
        label,
        form,
        state: "imported".into(),
    })
}

pub(crate) fn validate_headers(
    headers: &[String],
    allow_plaintext_secrets: bool,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for header in headers {
        if header.is_empty() || !seen.insert(header.clone()) {
            return Err("The migration file contains an empty or duplicate column header.".into());
        }
    }
    if seen.contains("extra_options") {
        return Err("The migration file cannot contain extra_options; configure trusted engine options in the application instead of importing executable command settings.".into());
    }

    let has_plaintext_passwords =
        seen.contains("source_password") || seen.contains("destination_password");
    if has_plaintext_passwords && !allow_plaintext_secrets {
        return Err(
            "Plaintext credential columns detected. Remove source_password and destination_password, use credential IDs instead, or set MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1 to import password material.".into()
        );
    }

    let missing = [
        "source_host",
        "source_user",
        "destination_host",
        "destination_user",
    ]
    .into_iter()
    .filter(|header| !seen.contains(*header))
    .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Missing required column(s): {}.",
            missing.join(", ")
        ))
    }
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
    use super::{
        job_from_values, open_import_workbook, parse_xlsx_cell_reference,
        plaintext_secrets_allowed, validate_xlsx_shared_strings,
        validate_xlsx_sheet_entry_dimensions,
    };
    use crate::migration_plan::Form;
    use calamine::{DataType, Reader};
    use std::collections::HashMap;
    use std::io::Cursor;
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
        assert_eq!(job.form.profile.name, "Acme Corp cutover");
        assert_eq!(job.label, "finance mailbox");
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

        assert!(job.form.source_password.is_empty());
        assert!(job.form.destination_password.is_empty());
    }
}
