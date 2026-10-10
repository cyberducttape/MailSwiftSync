//! Canonical integrity helpers shared by customer proofs and signing.

use crate::{core, plan_identity::snapshot_sha256};

pub(crate) fn evidence_digest(
    run_id: &str,
    plan_snapshot: &str,
    evidence: &core::MailboxEvidence,
) -> String {
    let canonical = format!(
        "run_id={run_id}\nplan_snapshot_sha256={}\nverification_method={}\nverification_outcome={}\nsource_folders={}\ndestination_folders={}\nsource_messages={}\ndestination_messages={}\nsource_bytes={}\ndestination_bytes={}\nunresolved_count={}\nfailed_messages={}\nauthoritative={}\nmissing_count={}\nextra_count={}\nmodified_count={}\nprobable_count={}\nmetadata_matched_count={}\n",
        snapshot_sha256(plan_snapshot),
        evidence.verification_method().as_str(),
        evidence.verification_outcome().as_str(),
        evidence.source_folders,
        evidence.destination_folders,
        evidence.source_messages,
        evidence.destination_messages,
        evidence.source_bytes,
        evidence.destination_bytes,
        evidence
            .unresolved_count()
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
        evidence.failed_messages,
        evidence.authoritative,
        evidence.missing_count(),
        evidence.extra_count(),
        evidence.modified_count(),
        evidence.probable_count(),
        evidence.metadata_matched_count(),
    );
    // Appended only when present so digests of evidence recorded before
    // flag verification existed are unchanged.
    let canonical = match evidence.flag_verification {
        Some(flags) => format!(
            "{canonical}flags_compared={}\nflags_mismatched={}\nflags_excepted={}\n",
            flags.compared_messages, flags.mismatched_messages, flags.excepted_messages
        ),
        None => canonical,
    };
    snapshot_sha256(&canonical)
}

/// Verification tier and coverage fields shared by operator reports and
/// customer proofs: which level was achieved, how many source messages were
/// individually checked, and the flag/keyword result (`null` when flags were
/// not verified, never an implied pass).
pub(crate) fn evidence_coverage_fields(
    evidence: &core::MailboxEvidence,
) -> serde_json::Map<String, serde_json::Value> {
    let (checked, total) = evidence.message_coverage();
    let mut fields = serde_json::Map::new();
    fields.insert(
        "verification_tier".into(),
        evidence.verification_level().into(),
    );
    fields.insert(
        "verification_status".into(),
        evidence.verification_status().into(),
    );
    fields.insert(
        "message_coverage".into(),
        serde_json::json!({
            "checked_messages": checked,
            "source_messages": total,
            "percent": core::coverage_percent(checked, total),
            "complete": total > 0 && checked == total,
        }),
    );
    fields.insert(
        "flag_verification".into(),
        evidence
            .flag_verification
            .map_or(serde_json::Value::Null, |flags| {
                serde_json::json!({
                    "compared_messages": flags.compared_messages,
                    "mismatched_messages": flags.mismatched_messages,
                    "excepted_messages": flags.excepted_messages,
                    "coverage_percent": core::coverage_percent(flags.compared_messages, evidence.source_messages),
                })
            }),
    );
    fields
}

/// Add a deterministic, bundle-level integrity reference to a structured
/// report. The digest is calculated over the canonical JSON representation
/// without the digest field itself, so whitespace changes do not invalidate a
/// proof while any semantic report change does.
pub(crate) fn with_proof_digest(mut value: serde_json::Value) -> Result<serde_json::Value, String> {
    {
        let object = value
            .as_object_mut()
            .ok_or("Migration proof must be a JSON object.")?;
        object.remove("proof_digest");
        // Re-signing must produce the same digest as signing an unsigned report.
        // The prior signature is metadata, not part of the signed report payload.
        object.remove("proof_signature");
    }
    let canonical = serde_json::to_string(&value).map_err(|error| error.to_string())?;
    let digest = snapshot_sha256(&canonical);
    value
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?
        .insert("proof_digest".into(), serde_json::Value::String(digest));
    Ok(value)
}

/// A JSON array produced on demand, one element at a time. It is generated
/// twice by `write_streamed_proof` (once for the digest, once for the file),
/// so it must yield the same elements both times.
pub(crate) type LazyArray<'a> =
    &'a dyn Fn(&mut dyn FnMut(serde_json::Value) -> Result<(), String>) -> Result<(), String>;

/// Write a proof object whose large arrays are never materialized. The bytes
/// hashed for `proof_digest` are exactly those `with_proof_digest` hashes for
/// the equivalent in-memory object: compact JSON with object keys in sorted
/// order. Only one array element is resident at a time.
pub(crate) fn write_streamed_proof(
    path: &std::path::Path,
    fields: serde_json::Map<String, serde_json::Value>,
    arrays: &[(&str, LazyArray<'_>)],
) -> Result<(), String> {
    if fields.contains_key("proof_digest")
        || fields.contains_key("proof_signature")
        || arrays.iter().any(|(key, _)| fields.contains_key(*key))
    {
        return Err("A streamed proof has a duplicated or reserved field.".into());
    }
    let mut hasher = HashingWriter(sha2::Sha256::default());
    serde_json::to_writer(
        &mut hasher,
        &StreamedProof {
            fields: &fields,
            arrays,
            digest: None,
        },
    )
    .map_err(|error| error.to_string())?;
    let digest = crate::reports::integrity::hex_encode(&sha2::Digest::finalize(hasher.0));
    let mut failure = None;
    crate::atomic_artifact::write_private_atomic_with(path, |file| {
        serde_json::to_writer_pretty(
            file,
            &StreamedProof {
                fields: &fields,
                arrays,
                digest: Some(&digest),
            },
        )
        .map_err(|error| {
            let message = error.to_string();
            failure = Some(message.clone());
            std::io::Error::other(message)
        })
    })
    .map_err(|error| failure.take().unwrap_or_else(|| error.to_string()))
}

struct HashingWriter(sha2::Sha256);

impl std::io::Write for HashingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        sha2::Digest::update(&mut self.0, bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct StreamedProof<'a> {
    fields: &'a serde_json::Map<String, serde_json::Value>,
    arrays: &'a [(&'a str, LazyArray<'a>)],
    digest: Option<&'a str>,
}

impl serde::Serialize for StreamedProof<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // Sorted keys, exactly as a serde_json map serializes them.
        let mut keys = self
            .fields
            .keys()
            .map(String::as_str)
            .chain(self.arrays.iter().map(|(key, _)| *key))
            .chain(self.digest.map(|_| "proof_digest"))
            .collect::<Vec<_>>();
        keys.sort_unstable();
        let mut map = serializer.serialize_map(Some(keys.len()))?;
        for key in keys {
            if let Some(value) = self.fields.get(key) {
                map.serialize_entry(key, value)?;
            } else if let Some((_, array)) = self.arrays.iter().find(|(name, _)| *name == key) {
                map.serialize_entry(key, &LazySequence(*array))?;
            } else if let Some(digest) = self.digest {
                map.serialize_entry(key, digest)?;
            }
        }
        map.end()
    }
}

struct LazySequence<'a>(LazyArray<'a>);

impl serde::Serialize for LazySequence<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error as _, SerializeSeq};
        let mut sequence = serializer.serialize_seq(None)?;
        let mut element_error = None;
        let produced = (self.0)(&mut |value| {
            sequence.serialize_element(&value).map_err(|error| {
                let message = error.to_string();
                element_error = Some(error);
                message
            })
        });
        if let Some(error) = element_error {
            return Err(error);
        }
        produced.map_err(S::Error::custom)?;
        sequence.end()
    }
}

pub(crate) fn canonical_signed_proof_payload(value: &serde_json::Value) -> Result<String, String> {
    let mut unsigned = value.clone();
    let signature = unsigned
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?
        .get_mut("proof_signature");
    if let Some(signature) = signature {
        signature
            .as_object_mut()
            .ok_or("Migration proof signature must be an object.")?
            .remove("signature");
    }
    serde_json::to_string(&unsigned).map_err(|error| error.to_string())
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn hex_decode<const N: usize>(value: &str) -> Result<[u8; N], String> {
    if value.len() != N * 2 {
        return Err(format!("expected {} hexadecimal bytes", N));
    }
    let mut output = [0_u8; N];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk).map_err(|_| "invalid hexadecimal text".to_owned())?;
        output[index] = u8::from_str_radix(text, 16)
            .map_err(|_| "invalid hexadecimal signature material".to_owned())?;
    }
    Ok(output)
}

#[cfg(test)]
mod streamed_proof_tests {
    use super::*;

    #[test]
    fn streamed_proofs_carry_exactly_the_in_memory_digest_and_verify() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("proof.json");
        let serde_json::Value::Object(fields) = serde_json::json!({
            "format": "mailswiftsync-customer-proof",
            "zeta": {"nested": [1, 2, {"b": true, "a": null}]},
            "alpha": "é ünïcode \u{1F4E7}",
            "number": 18_446_744_073_709_551_615_u64,
        }) else {
            unreachable!()
        };
        let items = (0..2_500)
            .map(|index| serde_json::json!({"index": index, "label": format!("mailbox {index}")}))
            .collect::<Vec<_>>();
        let emit_items = |emit: &mut dyn FnMut(serde_json::Value) -> Result<(), String>| {
            items.iter().cloned().try_for_each(emit)
        };
        let emit_none = |_: &mut dyn FnMut(serde_json::Value) -> Result<(), String>| Ok(());
        write_streamed_proof(
            &path,
            fields.clone(),
            &[("mailboxes", &emit_items), ("runs", &emit_none)],
        )
        .unwrap();

        let mut expected = serde_json::Value::Object(fields);
        expected["mailboxes"] = serde_json::Value::Array(items.clone());
        expected["runs"] = serde_json::json!([]);
        let expected = with_proof_digest(expected).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written, expected);
        crate::reports::signing::verify_file(&path, None).unwrap();

        // An element that fails to generate aborts the export; no file
        // replaces the previous artifact.
        let failing = |emit: &mut dyn FnMut(serde_json::Value) -> Result<(), String>| {
            emit(serde_json::json!(1))?;
            Err("missing evidence run".to_owned())
        };
        let error = write_streamed_proof(&path, serde_json::Map::new(), &[("mailboxes", &failing)])
            .unwrap_err();
        assert!(error.contains("missing evidence run"), "{error}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
                .unwrap(),
            expected
        );
        let _ = std::fs::remove_dir_all(directory);
    }
}
