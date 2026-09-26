//! Untrusted mailbox-list import and structural validation.
//!
//! Parsing is deliberately independent from egui state. This module owns the
//! worker thread, file limits, worksheet selection, row validation, and
//! conversion into import jobs; the UI owns only queue presentation and
//! result application.

use crate::{Form, SecretString};
use calamine::{Reader, Sheets, Xls, Xlsx, open_workbook_from_rs};
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

fn validate_xlsx_sheet_entry_dimensions<R: Read>(entry: &mut R) -> Result<(), String> {
    const MAX_DIMENSION_PREFIX_BYTES: usize = 128 * 1024;
    let mut prefix = Vec::with_capacity(128 * 1024);
    let mut chunk = [0_u8; 8192];
    while prefix.len() < MAX_DIMENSION_PREFIX_BYTES {
        let count = entry
            .read(&mut chunk)
            .map_err(|error| format!("Could not inspect worksheet dimensions: {error}"))?;
        if count == 0 {
            break;
        }
        prefix.extend_from_slice(&chunk[..count]);
        if prefix.windows(9).any(|window| window == b"<sheetData") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&prefix);
    let Some(start) = text.find("<dimension") else {
        let nonempty_sheet_data = text.find("<sheetData").is_some_and(|offset| {
            let remainder = &text[offset..];
            !remainder.starts_with("<sheetData/>") && !remainder.starts_with("<sheetData />")
        });
        if nonempty_sheet_data || prefix.len() >= MAX_DIMENSION_PREFIX_BYTES {
            return Err(
                "The worksheet does not declare a bounded early dimension; refusing to materialize it."
                    .into(),
            );
        }
        return Ok(());
    };
    let Some(reference_start) = text[start..].find("ref=") else {
        return Ok(());
    };
    let reference = text[start + reference_start + 4..].trim_start();
    let reference = reference
        .strip_prefix('"')
        .or_else(|| reference.strip_prefix('\''))
        .unwrap_or(reference);
    let end = reference.find(['"', '\'']).unwrap_or(reference.len());
    let endpoint = reference[..end].split(':').next_back().unwrap_or("");
    let (columns, rows) = parse_xlsx_cell_reference(endpoint)?;
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

#[cfg(test)]
fn validate_workbook_input(path: &Path) -> Result<(), String> {
    open_import_workbook(path).map(drop)
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
        "xls" => {
            const OLE_HEADER: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
            let mut file = file;
            let mut header = [0; OLE_HEADER.len()];
            file.read_exact(&mut header)
                .map_err(|error| format!("Could not read XLS import header: {error}"))?;
            if header != OLE_HEADER {
                return Err("The XLS workbook is not a valid legacy BIFF/OLE file.".into());
            }
            file.seek(SeekFrom::Start(0))
                .map_err(|error| format!("Could not rewind XLS import: {error}"))?;
            open_workbook_from_rs::<Xls<_>, _>(BufReader::new(file))
                .map(Sheets::Xls)
                .map_err(|error| {
                    format!(
                        "The XLS workbook could not be parsed as a legacy BIFF workbook: {error}"
                    )
                })
        }
        _ => Err("Choose a .xls or .xlsx workbook.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        job_from_values, parse_xlsx_cell_reference, plaintext_secrets_allowed,
        validate_workbook_input, validate_xlsx_sheet_entry_dimensions,
    };
    use crate::migration_plan::Form;
    use calamine::{DataType, Reader};
    use std::collections::HashMap;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn write_minimal_xlsx(path: &std::path::Path) {
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
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>source_host</t></is></c></row></sheetData></worksheet>"#,
            ),
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
    }

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
    fn xlsx_validation_and_calamine_reads_the_same_open_file() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-snapshot-{}.xlsx",
            uuid::Uuid::new_v4()
        ));
        write_minimal_xlsx(&path);
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
