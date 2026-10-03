//! Generic snapshot comparison for migration assurance.
//!
//! Provider collectors stay outside this module. They export named resource
//! arrays (mailboxes, messages, DNS, SSL, permissions, and so on), while this
//! module provides one deterministic comparison and proof format.

use crate::{atomic_artifact::write_private_atomic, reports::integrity::with_proof_digest};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::io::Write;
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAX_DETAILS: usize = 1_000;
const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;

const SOURCE_SIDE: i64 = 0;
const DESTINATION_SIDE: i64 = 1;

struct LimitedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            let mut extra = [0u8; 1];
            return match self.inner.read(&mut extra)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "snapshot exceeds the configured byte limit",
                )),
            };
        }
        let limit = buffer.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buffer[..limit])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

struct StagingDatabase {
    connection: Connection,
    _path: PathBuf,
    _cleanup: crate::credentials::CleanupGuard,
}

/// Stage snapshots in an owner-only on-disk SQLite database rather than an
/// in-memory database. The JSON parser retains only one record at a time;
/// SQLite may spill indexed payloads to the private runtime directory.
fn create_staging_database() -> Result<StagingDatabase, String> {
    let directory = crate::credentials::create_secret_directory()?;
    let cleanup = crate::credentials::CleanupGuard::new(vec![directory.clone()]);
    // SQLite's NOFOLLOW flag rejects a filename if any component is a symlink.
    // macOS temp paths commonly begin with /var, which aliases /private/var;
    // resolve the already-private run directory before creating/opening the DB
    // so NOFOLLOW can remain enabled without weakening the filesystem check.
    #[cfg(unix)]
    let database_directory = fs::canonicalize(&directory)
        .map_err(|error| format!("could not resolve private migration-audit directory: {error}"))?;
    #[cfg(not(unix))]
    let database_directory = directory.clone();
    crate::credentials::verify_private_directory(&database_directory)
        .map_err(|error| format!("could not verify private migration-audit directory: {error}"))?;
    let path = database_directory.join("migration-audit.sqlite3");
    crate::credentials::open_secret_file(&path)
        .map(drop)
        .map_err(|error| {
            format!("could not create private migration-audit staging database: {error}")
        })?;
    let connection = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| format!("could not open migration-audit staging database: {error}"))?;
    connection
        .execute_batch(
            "PRAGMA temp_store=FILE;
             PRAGMA journal_mode=DELETE;
             CREATE TABLE records(
                 side INTEGER NOT NULL,
                 category TEXT NOT NULL,
                 identity TEXT NOT NULL,
                 payload TEXT NOT NULL
             );
             CREATE INDEX records_lookup ON records(side, category, identity, payload);",
        )
        .map_err(|error| error.to_string())?;
    Ok(StagingDatabase {
        connection,
        _path: path,
        _cleanup: cleanup,
    })
}

struct SnapshotVisitor<'a> {
    connection: &'a mut Connection,
    side: i64,
    categories: &'a mut std::collections::BTreeSet<String>,
}

impl<'de> Visitor<'de> for SnapshotVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a migration snapshot JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        while let Some(category) = map.next_key::<String>()? {
            if matches!(category.as_str(), "metadata" | "format" | "format_version") {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            self.categories.insert(category.clone());
            map.next_value_seed(CategorySeed {
                connection: self.connection,
                side: self.side,
                category,
            })?;
        }
        Ok(())
    }
}

struct CategorySeed<'a> {
    connection: &'a mut Connection,
    side: i64,
    category: String,
}

impl<'de> Visitor<'de> for CategorySeed<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON array of migration records")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let transaction = self
            .connection
            .transaction()
            .map_err(serde::de::Error::custom)?;
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO records(side,category,identity,payload) VALUES(?1,?2,?3,?4)",
            )
            .map_err(serde::de::Error::custom)?;
        while let Some(item) = sequence.next_element::<Value>()? {
            let (identity, _) =
                identity(&self.category, &item).map_err(serde::de::Error::custom)?;
            let payload = serde_json::to_string(&item).map_err(serde::de::Error::custom)?;
            insert
                .execute(params![self.side, self.category, identity, payload])
                .map_err(serde::de::Error::custom)?;
        }
        drop(insert);
        transaction.commit().map_err(serde::de::Error::custom)?;
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for CategorySeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(self)
    }
}

fn stage_snapshot(
    path: &Path,
    label: &str,
    side: i64,
    connection: &mut Connection,
    categories: &mut std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let file = fs::File::open(path).map_err(|error| format!("{label} snapshot: {error}"))?;
    if file
        .metadata()
        .map_err(|error| format!("{label} snapshot: {error}"))?
        .len()
        > MAX_SNAPSHOT_BYTES
    {
        return Err(format!(
            "{label} snapshot exceeds the {MAX_SNAPSHOT_BYTES}-byte limit"
        ));
    }
    let reader = LimitedReader {
        inner: file,
        remaining: MAX_SNAPSHOT_BYTES,
    };
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    deserializer
        .deserialize_map(SnapshotVisitor {
            connection,
            side,
            categories,
        })
        .map_err(|error| format!("invalid {label} snapshot: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("invalid {label} snapshot: {error}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditResult {
    pub report: Value,
    pub differences: usize,
}

#[cfg(test)]
fn digest(value: &Value) -> String {
    let mut hasher = Sha256::new();
    let mut writer = DigestWriter(&mut hasher);
    serde_json::to_writer(&mut writer, value).expect("JSON values are serializable");
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
struct DigestWriter<'a>(&'a mut Sha256);

#[cfg(test)]
impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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
    let mut detail_groups: usize = 0;
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
        let mut source_value = None;
        for item in &left {
            let key = serde_json::to_string(item).unwrap_or_default();
            if let Some(count) = right_counts.get_mut(&key)
                && *count > 0
            {
                *count -= 1;
                exact += 1;
            } else if source_value.is_none() {
                source_value = Some(item);
            }
        }
        let destination_value = right.iter().find(|item| {
            right_counts
                .get(&serde_json::to_string(item).unwrap_or_default())
                .is_some_and(|count| *count > 0)
        });
        let modified = (left.len() - exact).min(right.len() - exact);
        let missing = left.len() - exact - modified;
        let extra = right.len() - exact - modified;
        matched += exact;
        differences += missing + extra + modified;
        missing_total += missing;
        extra_total += extra;
        modified_total += modified;
        if missing > 0 || extra > 0 || modified > 0 {
            detail_groups += 1;
        }
        if details.len() < MAX_DETAILS && (missing > 0 || extra > 0 || modified > 0) {
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
    let detail_count = details.len();
    let details_omitted = detail_groups.saturating_sub(detail_count);
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

fn staged_digest(connection: &Connection, side: i64, category: &str) -> Result<String, String> {
    let mut statement = connection
        .prepare(
            "SELECT payload FROM records
             WHERE side=?1 AND category=?2
             ORDER BY identity, payload",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = statement
        .query(params![side, category])
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(b"[");
    let mut first = true;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        if !first {
            hasher.update(b",");
        }
        first = false;
        let payload: String = row.get(0).map_err(|error| error.to_string())?;
        hasher.update(payload.as_bytes());
    }
    hasher.update(b"]");
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn staged_unmatched_representative(
    connection: &Connection,
    side: i64,
    other_side: i64,
    category: &str,
    identity: &str,
) -> Result<Option<String>, String> {
    connection
        .query_row(
            "SELECT own.payload
             FROM records AS own
             LEFT JOIN (
                 SELECT payload, COUNT(*) AS count
                 FROM records
                 WHERE side=?4 AND category=?2 AND identity=?3
                 GROUP BY payload
             ) AS other ON other.payload=own.payload
             WHERE own.side=?1 AND own.category=?2 AND own.identity=?3
             GROUP BY own.payload, other.count
             HAVING COUNT(*) > COALESCE(other.count, 0)
             ORDER BY own.payload LIMIT 1",
            params![side, category, identity, other_side],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())
}

fn compare_staged_category(
    connection: &Connection,
    category: &str,
) -> Result<(Value, usize), String> {
    let source_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM records WHERE side=?1 AND category=?2",
            params![SOURCE_SIDE, category],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?
        .try_into()
        .map_err(|_| "source record count exceeds supported range".to_owned())?;
    let destination_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM records WHERE side=?1 AND category=?2",
            params![DESTINATION_SIDE, category],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?
        .try_into()
        .map_err(|_| "destination record count exceeds supported range".to_owned())?;

    let mut statement = connection
        .prepare(
            "WITH source_groups AS (
                 SELECT identity, COUNT(*) AS count
                 FROM records WHERE side=?1 AND category=?2
                 GROUP BY identity
             ),
             destination_groups AS (
                 SELECT identity, COUNT(*) AS count
                 FROM records WHERE side=?3 AND category=?2
                 GROUP BY identity
             ),
             identities AS (
                 SELECT identity FROM source_groups
                 UNION
                 SELECT identity FROM destination_groups
             ),
             source_payloads AS (
                 SELECT identity, payload, COUNT(*) AS count
                 FROM records WHERE side=?1 AND category=?2
                 GROUP BY identity, payload
             ),
             destination_payloads AS (
                 SELECT identity, payload, COUNT(*) AS count
                 FROM records WHERE side=?3 AND category=?2
                 GROUP BY identity, payload
             ),
             exact_groups AS (
                 SELECT source_payloads.identity,
                        SUM(
                            CASE WHEN source_payloads.count < destination_payloads.count
                                 THEN source_payloads.count
                                 ELSE destination_payloads.count END
                        ) AS exact
                 FROM source_payloads
                 JOIN destination_payloads
                   ON destination_payloads.identity=source_payloads.identity
                  AND destination_payloads.payload=source_payloads.payload
                 GROUP BY source_payloads.identity
             )
             SELECT identities.identity,
                    COALESCE(source_groups.count, 0),
                    COALESCE(destination_groups.count, 0),
                    COALESCE(exact_groups.exact, 0)
             FROM identities
             LEFT JOIN source_groups ON source_groups.identity=identities.identity
             LEFT JOIN destination_groups ON destination_groups.identity=identities.identity
             LEFT JOIN exact_groups ON exact_groups.identity=identities.identity
             ORDER BY identities.identity",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = statement
        .query(params![SOURCE_SIDE, category, DESTINATION_SIDE])
        .map_err(|error| error.to_string())?;
    let mut details = Vec::new();
    let mut differences = 0usize;
    let mut matched = 0usize;
    let mut missing_total = 0usize;
    let mut extra_total = 0usize;
    let mut modified_total = 0usize;
    let mut detail_groups = 0usize;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let identity: String = row.get(0).map_err(|error| error.to_string())?;
        let source: usize = row
            .get::<_, i64>(1)
            .map_err(|error| error.to_string())?
            .try_into()
            .map_err(|_| "source identity count exceeds supported range".to_owned())?;
        let destination: usize = row
            .get::<_, i64>(2)
            .map_err(|error| error.to_string())?
            .try_into()
            .map_err(|_| "destination identity count exceeds supported range".to_owned())?;
        let exact: usize = row
            .get::<_, i64>(3)
            .map_err(|error| error.to_string())?
            .try_into()
            .map_err(|_| "exact identity count exceeds supported range".to_owned())?;
        let modified = (source - exact).min(destination - exact);
        let missing = source - exact - modified;
        let extra = destination - exact - modified;
        matched += exact;
        differences += missing + extra + modified;
        missing_total += missing;
        extra_total += extra;
        modified_total += modified;
        if missing > 0 || extra > 0 || modified > 0 {
            detail_groups += 1;
            if details.len() < MAX_DETAILS {
                let source_payload = staged_unmatched_representative(
                    connection,
                    SOURCE_SIDE,
                    DESTINATION_SIDE,
                    category,
                    &identity,
                )?;
                let destination_payload = staged_unmatched_representative(
                    connection,
                    DESTINATION_SIDE,
                    SOURCE_SIDE,
                    category,
                    &identity,
                )?;
                details.push(serde_json::json!({
                    "identity": identity,
                    "status": if missing > 0 && extra == 0 { "missing" } else if extra > 0 && missing == 0 { "extra" } else { "modified" },
                    "missing": missing,
                    "extra": extra,
                    "modified": modified,
                    "source_sha256": source_payload.as_deref().map(|payload| {
                        let mut hasher = Sha256::new();
                        hasher.update(payload.as_bytes());
                        hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>()
                    }),
                    "destination_sha256": destination_payload.as_deref().map(|payload| {
                        let mut hasher = Sha256::new();
                        hasher.update(payload.as_bytes());
                        hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>()
                    }),
                }));
            }
        }
    }
    let detail_count = details.len();
    let details_omitted = detail_groups.saturating_sub(detail_count);
    let identity_policy = if identity_schema(category).is_some() {
        "typed"
    } else {
        "heuristic"
    };
    Ok((
        serde_json::json!({
            "source_count": source_count, "destination_count": destination_count,
            "matched": matched, "missing": missing_total, "extra": extra_total,
            "modified": modified_total,
            "source_sha256": staged_digest(connection, SOURCE_SIDE, category)?,
            "destination_sha256": staged_digest(connection, DESTINATION_SIDE, category)?,
            "identity_policy": identity_policy,
            "details": details,
            "detail_count": detail_count,
            "details_truncated": details_omitted > 0,
            "details_omitted": details_omitted,
        }),
        differences,
    ))
}

fn compare_staged(
    connection: &Connection,
    categories: &std::collections::BTreeSet<String>,
) -> Result<AuditResult, String> {
    let mut category_reports = Map::new();
    let mut differences = 0usize;
    for category in categories {
        let (report, count) = compare_staged_category(connection, category)
            .map_err(|error| format!("category '{category}': {error}"))?;
        differences += count;
        category_reports.insert(category.clone(), report);
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

#[cfg(test)]
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
    let mut connection = create_staging_database()?;
    let mut categories = std::collections::BTreeSet::new();
    stage_snapshot(
        source,
        "source",
        SOURCE_SIDE,
        &mut connection.connection,
        &mut categories,
    )?;
    stage_snapshot(
        destination,
        "destination",
        DESTINATION_SIDE,
        &mut connection.connection,
        &mut categories,
    )?;
    let result = compare_staged(&connection.connection, &categories)?;
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
    fn duplicate_mismatches_report_omitted_groups_not_omitted_records() {
        let source = serde_json::json!({"widgets": (0..1_001)
            .map(|value| serde_json::json!({"id":"same", "value":value}))
            .collect::<Vec<_>>()});
        let destination = serde_json::json!({"widgets": (0..1_001)
            .map(|value| serde_json::json!({"id":"same", "value":value + 1_001}))
            .collect::<Vec<_>>()});
        let result = compare(&source, &destination).unwrap();
        let widgets = &result.report["categories"]["widgets"];
        assert_eq!(widgets["modified"], 1_001);
        assert_eq!(widgets["detail_count"], 1);
        assert_eq!(widgets["details_omitted"], 0);
        assert_eq!(widgets["details_truncated"], false);
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

    #[test]
    fn compare_files_streams_snapshots_through_sqlite_staging() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-migrate-audit-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&directory, permissions).unwrap();
        }
        let source = directory.join("source.json");
        let destination = directory.join("destination.json");
        let output = directory.join("report.json");
        std::fs::write(
            &source,
            r#"{"messages":[{"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"value":"same"}],"metadata":{"collector":"source"}}"#,
        )
        .unwrap();
        std::fs::write(
            &destination,
            r#"{"metadata":{"collector":"destination"},"messages":[{"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"value":"same"}]}"#,
        )
        .unwrap();
        let result = compare_files(&source, &destination, &output).unwrap();
        assert_eq!(result.differences, 0);
        assert_eq!(result.report["verdict"], "pass");
        assert!(result.report["proof_digest"].as_str().is_some());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn streamed_comparison_matches_reference_for_duplicate_and_modified_records() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-migrate-audit-differential-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&directory, permissions).unwrap();
        }

        let cases = [
            (
                serde_json::json!({
                    "messages": [
                        {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"body":"same"},
                        {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"body":"source-only"},
                        {"account":"a","folder":"INBOX","uidvalidity":1,"uid":2,"body":"missing"}
                    ],
                    "widgets": [{"id":"duplicate","value":1},{"id":"duplicate","value":2}]
                }),
                serde_json::json!({
                    "widgets": [{"id":"duplicate","value":1},{"id":"duplicate","value":3}],
                    "messages": [
                        {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"body":"same"},
                        {"account":"a","folder":"INBOX","uidvalidity":1,"uid":1,"body":"destination-only"},
                        {"account":"a","folder":"INBOX","uidvalidity":1,"uid":3,"body":"extra"}
                    ]
                }),
            ),
            (
                serde_json::json!({"files":[{"path":"a/../b","sha256":"one"},{"path":"x","sha256":"two"}]}),
                serde_json::json!({"files":[{"path":"b","sha256":"changed"},{"path":"y","sha256":"three"}]}),
            ),
            (
                serde_json::json!({"widgets":[{"id":"same","value":7},{"id":"same","value":7}]}),
                serde_json::json!({"widgets":[{"id":"same","value":7}]}),
            ),
        ];

        for (index, (source_value, destination_value)) in cases.into_iter().enumerate() {
            let source = directory.join(format!("source-{index}.json"));
            let destination = directory.join(format!("destination-{index}.json"));
            let output = directory.join(format!("report-{index}.json"));
            std::fs::write(&source, serde_json::to_vec(&source_value).unwrap()).unwrap();
            std::fs::write(
                &destination,
                serde_json::to_vec(&destination_value).unwrap(),
            )
            .unwrap();

            let expected = compare(&source_value, &destination_value).unwrap();
            let streamed = compare_files(&source, &destination, &output).unwrap();
            assert_eq!(streamed.differences, expected.differences, "case {index}");
            assert_eq!(streamed.report, expected.report, "case {index}");
        }

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn staging_database_is_private_on_disk_and_removed_on_drop() {
        let staging = create_staging_database().unwrap();
        let path = staging._path.clone();
        let directory = path.parent().unwrap().to_owned();
        #[cfg(unix)]
        assert_eq!(fs::canonicalize(&directory).unwrap(), directory);
        #[cfg(not(unix))]
        assert!(fs::canonicalize(&directory).unwrap().is_dir());
        let databases: Vec<(String, String)> = staging
            .connection
            .prepare("PRAGMA database_list")
            .unwrap()
            .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            databases
                .iter()
                .any(|(name, file)| name == "main" && Path::new(file) == path)
        );
        assert!(
            staging
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='records'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
                > 0
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        drop(staging);
        assert!(!directory.exists());
    }
}
