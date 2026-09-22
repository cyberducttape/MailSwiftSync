//! Generic snapshot comparison for migration assurance.
//!
//! Provider collectors stay outside this module. They export named resource
//! arrays (mailboxes, messages, DNS, SSL, permissions, and so on), while this
//! module provides one deterministic comparison and proof format.

use crate::{atomic_artifact::write_private_atomic, reports::integrity::with_proof_digest};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

const MAX_DETAILS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditResult {
    pub report: Value,
    pub differences: usize,
}

fn digest(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("JSON values are serializable");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn identity(item: &Value) -> String {
    let Some(object) = item.as_object() else {
        return serde_json::to_string(item).unwrap_or_default();
    };
    for field in [
        "id",
        "key",
        "email",
        "address",
        "path",
        "message_id",
        "uid",
        "name",
    ] {
        if let Some(value) = object.get(field).filter(|value| !value.is_null()) {
            return format!(
                "{field}:{}",
                serde_json::to_string(value).unwrap_or_default()
            );
        }
    }
    serde_json::to_string(item).unwrap_or_default()
}

fn canonical_items(value: &Value) -> Result<Vec<Value>, String> {
    let items = value
        .as_array()
        .ok_or("Each snapshot category must be a JSON array.")?;
    let mut items = items.to_vec();
    items.sort_by_key(|item| {
        (
            identity(item),
            serde_json::to_string(item).unwrap_or_default(),
        )
    });
    Ok(items)
}

fn grouped(items: &[Value]) -> BTreeMap<String, Vec<Value>> {
    let mut result = BTreeMap::new();
    for item in items {
        result
            .entry(identity(item))
            .or_insert_with(Vec::new)
            .push(item.clone());
    }
    for values in result.values_mut() {
        values.sort_by_key(|value| serde_json::to_string(value).unwrap_or_default());
    }
    result
}

fn compare_category(source: &Value, destination: &Value) -> Result<(Value, usize), String> {
    let source = canonical_items(source)?;
    let destination = canonical_items(destination)?;
    let source_groups = grouped(&source);
    let destination_groups = grouped(&destination);
    let mut details = Vec::new();
    let mut differences = 0;
    let mut matched = 0;
    let mut missing_total = 0;
    let mut extra_total = 0;
    let mut modified_total = 0;
    let identities = source_groups
        .keys()
        .chain(destination_groups.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for key in identities {
        let left = source_groups.get(&key).cloned().unwrap_or_default();
        let right = destination_groups.get(&key).cloned().unwrap_or_default();
        let mut right_counts = BTreeMap::<String, usize>::new();
        for item in &right {
            *right_counts
                .entry(serde_json::to_string(item).unwrap_or_default())
                .or_default() += 1;
        }
        let mut exact = 0;
        for item in &left {
            let key = serde_json::to_string(item).unwrap_or_default();
            if let Some(count) = right_counts.get_mut(&key) {
                if *count > 0 {
                    *count -= 1;
                    exact += 1;
                }
            }
        }
        let modified = (left.len() - exact).min(right.len() - exact);
        let missing = left.len() - exact - modified;
        let extra = right.len() - exact - modified;
        matched += exact;
        differences += missing + extra + modified;
        missing_total += missing;
        extra_total += extra;
        modified_total += modified;
        if details.len() < MAX_DETAILS && (missing > 0 || extra > 0 || modified > 0) {
            let source_value = left.iter().find(|item| !right.contains(item));
            let destination_value = right.iter().find(|item| !left.contains(item));
            details.push(serde_json::json!({
                "identity": key,
                "status": if missing > 0 && extra == 0 { "missing" } else if extra > 0 && missing == 0 { "extra" } else { "modified" },
                "missing": missing,
                "extra": extra,
                "source_sha256": source_value.map(digest),
                "destination_sha256": destination_value.map(digest),
            }));
        }
    }
    Ok((
        serde_json::json!({
            "source_count": source.len(), "destination_count": destination.len(),
            "matched": matched, "missing": missing_total, "extra": extra_total,
            "modified": modified_total,
            "source_sha256": digest(&Value::Array(source)),
            "destination_sha256": digest(&Value::Array(destination)), "details": details,
        }),
        differences,
    ))
}

pub(crate) fn compare(source: &Value, destination: &Value) -> Result<AuditResult, String> {
    let source = source
        .as_object()
        .ok_or("Source snapshot must be a JSON object.")?;
    let destination = destination
        .as_object()
        .ok_or("Destination snapshot must be a JSON object.")?;
    let categories = source
        .keys()
        .chain(destination.keys())
        .filter(|key| !matches!(key.as_str(), "metadata" | "format" | "format_version"))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut category_reports = Map::new();
    let mut differences = 0;
    for category in categories {
        let empty_left = Value::Array(Vec::new());
        let empty_right = Value::Array(Vec::new());
        let left = source.get(&category).unwrap_or(&empty_left);
        let right = destination.get(&category).unwrap_or(&empty_right);
        let (report, count) = compare_category(left, right)
            .map_err(|error| format!("category '{category}': {error}"))?;
        differences += count;
        category_reports.insert(category, report);
    }
    let report = with_proof_digest(serde_json::json!({
        "format": "mailswiftsync-migration-assurance", "format_version": 1,
        "verdict": if differences == 0 { "pass" } else { "fail" },
        "difference_count": differences, "categories": category_reports,
        "note": "This report proves equality of the supplied snapshots. It does not claim either snapshot is complete unless the collector documents its scope."
    }))?;
    Ok(AuditResult {
        report,
        differences,
    })
}

pub(crate) fn compare_files(
    source: &Path,
    destination: &Path,
    output: &Path,
) -> Result<AuditResult, String> {
    let source: Value =
        serde_json::from_str(&fs::read_to_string(source).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid source snapshot: {e}"))?;
    let destination: Value =
        serde_json::from_str(&fs::read_to_string(destination).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid destination snapshot: {e}"))?;
    let result = compare(&source, &destination)?;
    let text = serde_json::to_string_pretty(&result.report).map_err(|e| e.to_string())?;
    write_private_atomic(output, &text).map_err(|e| e.to_string())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_missing_extra_and_modified_records() {
        let source =
            serde_json::json!({"messages":[{"id":"one","sha256":"a"},{"id":"two","sha256":"b"}]});
        let destination = serde_json::json!({"messages":[{"id":"one","sha256":"changed"},{"id":"three","sha256":"c"}]});
        let result = compare(&source, &destination).unwrap();
        assert_eq!(result.differences, 3);
        assert_eq!(result.report["verdict"], "fail");
        assert_eq!(result.report["categories"]["messages"]["missing"], 1);
        assert_eq!(result.report["categories"]["messages"]["extra"], 1);
        assert_eq!(result.report["categories"]["messages"]["modified"], 1);
    }
    #[test]
    fn category_order_does_not_change_verdict() {
        let a = serde_json::json!({"folders":[{"id":"b"},{"id":"a"}]});
        let b = serde_json::json!({"folders":[{"id":"a"},{"id":"b"}]});
        assert_eq!(compare(&a, &b).unwrap().differences, 0);
    }

    #[test]
    fn duplicate_identities_are_compared_as_multisets() {
        let source = serde_json::json!({"messages":[{"id":"same"},{"id":"same"}]});
        let destination = serde_json::json!({"messages":[{"id":"same"}]});
        let result = compare(&source, &destination).unwrap();
        assert_eq!(result.differences, 1);
        assert_eq!(result.report["categories"]["messages"]["missing"], 1);
    }
}
