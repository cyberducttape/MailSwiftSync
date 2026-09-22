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

#[derive(Debug, Clone, Copy)]
struct IdentitySchema {
    name: &'static str,
    fields: &'static [&'static str],
}

fn identity_schema(category: &str) -> Option<IdentitySchema> {
    match category {
        "mailbox" | "mailboxes" => Some(IdentitySchema {
            name: "account+mailbox",
            fields: &["account", "mailbox"],
        }),
        "message" | "messages" => Some(IdentitySchema {
            name: "account+folder+uidvalidity+uid",
            fields: &["account", "folder", "uidvalidity", "uid"],
        }),
        "dns" => Some(IdentitySchema {
            name: "zone+owner+type+value",
            fields: &["zone", "owner", "type", "value"],
        }),
        "file" | "files" => Some(IdentitySchema {
            name: "normalized_path",
            fields: &["path"],
        }),
        "database" | "databases" => Some(IdentitySchema {
            name: "server+database+object",
            fields: &["server", "database", "object"],
        }),
        _ => None,
    }
}

fn heuristic_identity(item: &Value) -> String {
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

fn typed_identity(item: &Value, schema: IdentitySchema) -> Result<String, String> {
    let object = item.as_object().ok_or_else(|| {
        format!(
            "records in this category must be JSON objects (identity schema {})",
            schema.name
        )
    })?;
    let mut values = Vec::with_capacity(schema.fields.len());
    for field in schema.fields {
        let value = object
            .get(*field)
            .filter(|value| !value.is_null())
            .ok_or_else(|| {
                format!(
                    "record is missing required identity field '{field}' (schema {})",
                    schema.name
                )
            })?;
        let value = if *field == "path" {
            let path = value
                .as_str()
                .ok_or_else(|| "file path identity must be a string".to_owned())?;
            Value::String(normalize_path(path))
        } else {
            value.clone()
        };
        values.push(value);
    }
    Ok(serde_json::to_string(&values).expect("JSON values are serializable"))
}

fn normalize_path(path: &str) -> String {
    let mut components = Vec::new();
    for component in std::path::Path::new(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if components
                    .last()
                    .is_some_and(|value: &String| value != "..")
                {
                    components.pop();
                } else {
                    components.push("..".to_owned());
                }
            }
            std::path::Component::Normal(value) => {
                components.push(value.to_string_lossy().into_owned())
            }
            std::path::Component::RootDir => components.push(String::new()),
            std::path::Component::Prefix(prefix) => {
                components.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
        }
    }
    if components.len() == 1 && components[0].is_empty() {
        return "/".to_owned();
    }
    components.join("/")
}

fn identity(category: &str, item: &Value) -> Result<(String, &'static str), String> {
    if let Some(schema) = identity_schema(category) {
        return Ok((typed_identity(item, schema)?, "typed"));
    }
    Ok((heuristic_identity(item), "heuristic"))
}

fn canonical_items(category: &str, value: &Value) -> Result<Vec<Value>, String> {
    let items = value
        .as_array()
        .ok_or("Each snapshot category must be a JSON array.")?;
    let mut items = items.to_vec();
    items.sort_by_key(|item| {
        (
            identity(category, item)
                .map(|(value, _)| value)
                .unwrap_or_default(),
            serde_json::to_string(item).unwrap_or_default(),
        )
    });
    Ok(items)
}

fn grouped(category: &str, items: &[Value]) -> Result<BTreeMap<String, Vec<Value>>, String> {
    let mut result = BTreeMap::new();
    for item in items {
        let key = identity(category, item)?.0;
        result
            .entry(key)
            .or_insert_with(Vec::new)
            .push(item.clone());
    }
    for values in result.values_mut() {
        values.sort_by_key(|value| serde_json::to_string(value).unwrap_or_default());
    }
    Ok(result)
}

fn compare_category(
    category: &str,
    source: &Value,
    destination: &Value,
) -> Result<(Value, usize), String> {
    let source = canonical_items(category, source)?;
    let destination = canonical_items(category, destination)?;
    let source_groups = grouped(category, &source)?;
    let destination_groups = grouped(category, &destination)?;
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
            if let Some(count) = right_counts.get_mut(&key)
                && *count > 0
            {
                *count -= 1;
                exact += 1;
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
                "modified": modified,
                "source_sha256": source_value.map(digest),
                "destination_sha256": destination_value.map(digest),
            }));
        }
    }
    let total_detail_count = missing_total + extra_total + modified_total;
    let detail_count = details.len();
    let details_omitted = total_detail_count.saturating_sub(detail_count);
    let identity_policy = if identity_schema(category).is_some() {
        "typed"
    } else {
        "heuristic"
    };
    Ok((
        serde_json::json!({
            "source_count": source.len(), "destination_count": destination.len(),
            "matched": matched, "missing": missing_total, "extra": extra_total,
            "modified": modified_total,
            "source_sha256": digest(&Value::Array(source)),
            "destination_sha256": digest(&Value::Array(destination)),
            "identity_policy": identity_policy,
            "details": details,
            "detail_count": detail_count,
            "details_truncated": details_omitted > 0,
            "details_omitted": details_omitted,
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
        let (report, count) = compare_category(&category, left, right)
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
        let source = serde_json::json!({"messages":[
            {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"sha256":"a"},
            {"account":"a","folder":"INBOX","uidvalidity":1,"uid":2,"sha256":"b"}
        ]});
        let destination = serde_json::json!({"messages":[
            {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"sha256":"changed"},
            {"account":"a","folder":"INBOX","uidvalidity":1,"uid":3,"sha256":"c"}
        ]});
        let result = compare(&source, &destination).unwrap();
        assert_eq!(result.differences, 3);
        assert_eq!(result.report["verdict"], "fail");
        assert_eq!(result.report["categories"]["messages"]["missing"], 1);
        assert_eq!(result.report["categories"]["messages"]["extra"], 1);
        assert_eq!(result.report["categories"]["messages"]["modified"], 1);
        assert_eq!(
            result.report["categories"]["messages"]["identity_policy"],
            "typed"
        );
        assert_eq!(
            result.report["categories"]["messages"]["details"][0]["modified"],
            1
        );
    }
    #[test]
    fn category_order_does_not_change_verdict() {
        let a = serde_json::json!({"folders":[{"id":"b"},{"id":"a"}]});
        let b = serde_json::json!({"folders":[{"id":"a"},{"id":"b"}]});
        assert_eq!(compare(&a, &b).unwrap().differences, 0);
    }

    #[test]
    fn duplicate_identities_are_compared_as_multisets() {
        let source = serde_json::json!({"messages":[
            {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1},
            {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1}
        ]});
        let destination = serde_json::json!({"messages":[
            {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1}
        ]});
        let result = compare(&source, &destination).unwrap();
        assert_eq!(result.differences, 1);
        assert_eq!(result.report["categories"]["messages"]["missing"], 1);
    }

    #[test]
    fn reports_heuristic_identity_and_detail_truncation() {
        let source = serde_json::json!({
            "custom": (0..1_001).map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>()
        });
        let destination = serde_json::json!({"custom": []});
        let result = compare(&source, &destination).unwrap();
        let report = &result.report["categories"]["custom"];
        assert_eq!(report["identity_policy"], "heuristic");
        assert_eq!(report["detail_count"], 1_000);
        assert_eq!(report["details_truncated"], true);
        assert_eq!(report["details_omitted"], 1);
    }
}
