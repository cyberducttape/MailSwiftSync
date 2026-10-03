#![no_main]

#[path = "../../src/bulk_import/rows.rs"]
mod rows;

const MAX_COLUMNS: usize = 64;
const MAX_CELL_BYTES: usize = 64 * 1024;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut reader = csv::ReaderBuilder::new().from_reader(data);
    let Ok(raw_headers) = reader.headers() else {
        return;
    };
    let Ok(headers) = rows::normalize_headers(raw_headers.iter().map(str::to_owned), MAX_COLUMNS, "file") else {
        return;
    };
    assert_eq!(
        rows::normalize_headers(headers.clone(), MAX_COLUMNS, "file")
            .expect("normalization is idempotent"),
        headers
    );

    let valid_without_plaintext = rows::validate_headers(&headers, false).is_ok();
    let valid_with_plaintext = rows::validate_headers(&headers, true).is_ok();
    if valid_without_plaintext {
        assert!(valid_with_plaintext);
    }

    for (index, record) in reader.records().take(128).enumerate() {
        let Ok(record) = record else {
            continue;
        };
        if !valid_with_plaintext {
            continue;
        }
        if let Ok(values) = rows::record_values(
            &headers,
            record.iter(),
            index + 2,
            MAX_CELL_BYTES,
        ) {
            assert_eq!(values.len(), headers.len());
            assert!(values.values().all(|value| value.len() <= MAX_CELL_BYTES));
        }
    }
});
