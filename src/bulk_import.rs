//! Untrusted mailbox-list import and structural validation.
//!
//! Parsing is deliberately independent from egui state. This module owns the
//! worker thread, file limits, worksheet selection, row validation, and
//! conversion into import jobs; the UI owns only queue presentation and
//! result application.

use crate::{Form, SecretString};
use calamine::{Reader, Xls, open_workbook, open_workbook_auto};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

#[derive(Clone)]
pub(crate) struct BulkJob {
    pub(crate) label: String,
    pub(crate) form: Form,
    pub(crate) state: String,
}

pub(crate) struct PendingSheetImport {
    pub(crate) path: PathBuf,
    pub(crate) sheets: Vec<String>,
}

pub(crate) enum BulkImportResult {
    Jobs(Vec<BulkJob>),
    Workbook { path: PathBuf, sheets: Vec<String> },
}

/// Start a bounded/validated mailbox-file import away from the egui thread.
/// The caller receives only the future result; file-format dispatch and
/// parser ownership remain with the import domain.
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
        } else if ext == "xls" || ext == "xlsx" {
            workbook_sheets(&path).map(|sheets| BulkImportResult::Workbook { path, sheets })
        } else {
            Err("Choose a .csv, .xls, or .xlsx file.".into())
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
    validate_bulk_import_file(path)?;
    let mut reader = csv::Reader::from_path(path).map_err(|error| error.to_string())?;
    let headers = reader
        .headers()
        .map_err(|error| error.to_string())?
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let allow_plaintext_secrets = std::env::var("MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
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
    Ok(jobs)
}

pub(crate) fn workbook_sheets(path: &Path) -> Result<Vec<String>, String> {
    validate_workbook_input(path)?;
    let book = open_workbook_auto(path).map_err(|error| error.to_string())?;
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
    validate_workbook_input(path)?;
    let mut book = open_workbook_auto(path).map_err(|error| error.to_string())?;
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
    let allow_plaintext_secrets = std::env::var("MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
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
    // still opt-in. Normal CSV/XLS(X) imports are rejected earlier when the
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

pub(crate) fn validate_bulk_import_file(path: &Path) -> Result<(), String> {
    let size = std::fs::metadata(path)
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

fn validate_xlsx_container(path: &Path) -> Result<(), String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("Could not open XLSX import file: {error}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("The XLSX archive is invalid: {error}"))?;
    let entry_count = archive.len();
    validate_workbook_container_limits(entry_count, 0)?;
    let mut uncompressed_bytes = 0_u64;
    for index in 0..entry_count {
        let entry_size = archive
            .by_index(index)
            .map_err(|error| format!("Could not inspect XLSX archive entry: {error}"))?
            .size();
        uncompressed_bytes = uncompressed_bytes.saturating_add(entry_size);
        validate_workbook_container_limits(entry_count, uncompressed_bytes)?;
    }
    Ok(())
}

fn validate_workbook_input(path: &Path) -> Result<(), String> {
    validate_bulk_import_file(path)?;
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "xlsx" => validate_xlsx_container(path),
        "xls" => validate_legacy_xls_header(path),
        _ => Err("Choose a .xls or .xlsx workbook.".into()),
    }
}

/// Legacy BIFF workbooks are OLE compound files, not ZIP archives.  The
/// existing file-size limit still bounds the input before calamine opens it.
/// Check both the container signature and the actual BIFF parser here so an
/// invalid legacy workbook fails during validation with a useful error rather
/// than reaching the asynchronous import worker and failing later.
fn validate_legacy_xls_header(path: &Path) -> Result<(), String> {
    const OLE_HEADER: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("Could not open XLS import file: {error}"))?;
    let mut header = [0_u8; OLE_HEADER.len()];
    file.read_exact(&mut header)
        .map_err(|error| format!("Could not read XLS import header: {error}"))?;
    if header != OLE_HEADER {
        return Err("The XLS workbook is not a valid legacy BIFF/OLE file.".into());
    }
    open_workbook::<Xls<_>, _>(path)
        .map(|_| ())
        .map_err(|error| {
            format!("The XLS workbook could not be parsed as a legacy BIFF workbook: {error}")
        })
}

#[cfg(test)]
mod tests {
    use super::{job_from_values, validate_workbook_input};
    use crate::migration_plan::Form;
    use std::collections::HashMap;

    #[test]
    fn legacy_xls_input_must_be_a_parseable_biff_workbook() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-legacy-xls-header-{}-{}.xls",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let ole_header = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        std::fs::write(&path, ole_header).unwrap();

        let error = validate_workbook_input(&path).expect_err("header-only input must be rejected");
        assert!(error.contains("could not be parsed as a legacy BIFF workbook"));

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
