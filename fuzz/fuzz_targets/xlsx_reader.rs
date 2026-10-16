#![no_main]

use std::io::{Cursor, Read, Seek};

const MAX_BULK_IMPORT_CELL_BYTES: usize = 64 * 1024;
const MAX_BULK_IMPORT_COLUMNS: usize = 64;
const MAX_BULK_IMPORT_ROWS: usize = 100_000;
const MAX_BULK_IMPORT_SHARED_STRINGS: usize = 1_000_000;
const MAX_BULK_IMPORT_SHARED_STRING_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BULK_IMPORT_WORKSHEET_CELLS: usize = 1_000_000;

#[path = "../../src/bulk_import/xlsx_reference.rs"]
#[allow(dead_code)]
mod xlsx_reference;

fn validate_xlsx_archive<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<(), String> {
    if archive.len() > 128 {
        return Err("too many archive entries".into());
    }
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| error.to_string())?;
        total_size = total_size.saturating_add(entry.size());
        if total_size > 4 * 1024 * 1024 {
            return Err("archive expansion exceeds fuzz budget".into());
        }
    }
    Ok(())
}

fn validate_xlsx_sheet_entry_dimensions<R: Read>(entry: &mut R) -> Result<bool, String> {
    let mut bytes = Vec::new();
    entry
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("worksheet exceeds fuzz budget".into());
    }
    Ok(String::from_utf8_lossy(&bytes).contains("<worksheet"))
}

#[path = "../../src/bulk_import/xlsx.rs"]
mod xlsx;

fn fuzz_workbook(data: &[u8]) {
    let archive = match zip::ZipArchive::new(Cursor::new(data)) {
        Ok(archive) => archive,
        Err(_) => return,
    };
    let mut workbook = match xlsx::Workbook::new(archive) {
        Ok(workbook) => workbook,
        Err(_) => return,
    };
    for index in 0..workbook.sheet_names().len() {
        let _ = workbook.read_sheet(index);
    }
}

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    fuzz_workbook(data);
});
