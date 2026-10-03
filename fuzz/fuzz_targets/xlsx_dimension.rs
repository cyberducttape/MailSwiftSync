#![no_main]

#[path = "../../src/bulk_import/xlsx_reference.rs"]
mod xlsx_reference;

const MAX_IMPORT_COLUMNS: usize = 64;
const MAX_IMPORT_ROWS: u32 = 100_001;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let dimension = String::from_utf8_lossy(data);
    if xlsx_reference::validate_dimension_reference(
        &dimension,
        MAX_IMPORT_COLUMNS,
        MAX_IMPORT_ROWS,
    )
    .is_ok()
    {
        let mut endpoints = dimension.split(':');
        let (start_columns, start_rows) = xlsx_reference::parse_cell_reference(
            endpoints.next().expect("accepted dimensions have a start cell"),
        )
        .expect("accepted dimensions have a valid start cell");
        let (columns, rows) = if let Some(end) = endpoints.next() {
            xlsx_reference::parse_cell_reference(end)
                .expect("accepted dimensions have a valid end cell")
        } else {
            (start_columns, start_rows)
        };
        assert!(start_columns.max(columns) <= MAX_IMPORT_COLUMNS);
        assert!(start_rows.max(rows) <= MAX_IMPORT_ROWS);
        assert!(start_columns > 0 && start_rows > 0 && columns > 0 && rows > 0);
    }
});
