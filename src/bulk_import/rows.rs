//! Shared CSV/XLSX header and row normalization.

use std::collections::{HashMap, HashSet};

pub(crate) fn normalize_headers<I>(
    values: I,
    max_columns: usize,
    source_label: &str,
) -> Result<Vec<String>, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let headers = values
        .into_iter()
        .map(Into::into)
        .map(|header: String| header.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    if headers.len() > max_columns {
        return Err(format!(
            "The {source_label} has too many columns; the limit is {max_columns}."
        ));
    }
    Ok(headers)
}

pub(crate) fn record_values<I>(
    headers: &[String],
    values: I,
    row: usize,
    max_cell_bytes: usize,
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
            if value.len() > max_cell_bytes {
                return Err(format!(
                    "Row {row} contains a cell larger than {max_cell_bytes} bytes."
                ));
            }
            Ok((header.clone(), value))
        })
        .collect()
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
            "Plaintext credential columns detected. Remove source_password and destination_password or use credential IDs instead. To import passwords, set MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1 and confirm the warning for this import in the GUI.".into()
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
mod tests {
    use super::{normalize_headers, record_values, validate_headers};

    fn required_headers() -> Vec<String> {
        [
            "source_host",
            "source_user",
            "destination_host",
            "destination_user",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn headers_are_trimmed_and_ascii_case_normalized_before_validation() {
        let headers = normalize_headers(
            [
                " Source_Host ",
                "SOURCE_USER",
                "destination_host",
                "destination_user",
            ],
            4,
            "file",
        )
        .unwrap();
        assert_eq!(headers, required_headers());
        assert!(validate_headers(&headers, false).is_ok());
    }

    #[test]
    fn header_validation_rejects_duplicates_sensitive_options_and_unacknowledged_secrets() {
        let mut headers = required_headers();
        headers.push("source_host".into());
        assert!(validate_headers(&headers, true).is_err());

        headers = required_headers();
        headers.push("extra_options".into());
        assert!(validate_headers(&headers, true).is_err());

        headers = required_headers();
        headers.push("source_password".into());
        assert!(validate_headers(&headers, false).is_err());
        assert!(validate_headers(&headers, true).is_ok());
    }

    #[test]
    fn row_normalization_enforces_column_count_and_utf8_byte_bound() {
        let headers = required_headers();
        let row = ["source", "user", "destination", "user"];
        let values = record_values(&headers, row, 2, 16).unwrap();
        assert_eq!(values.len(), headers.len());
        assert!(record_values(&headers, ["one"], 2, 16).is_err());
        assert!(record_values(&headers, ["xx"; 4], 2, 1).is_err());

        assert!(normalize_headers(["a", "b"], 1, "worksheet").is_err());
    }
}
