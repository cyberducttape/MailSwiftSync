//! Canonical integrity helpers shared by customer proofs and signing.

use crate::{core, plan_identity::snapshot_sha256};

pub(crate) fn evidence_digest(
    run_id: &str,
    plan_snapshot: &str,
    evidence: &core::MailboxEvidence,
) -> String {
    let canonical = format!(
        "run_id={run_id}\nplan_snapshot_sha256={}\nsource_folders={}\ndestination_folders={}\nsource_messages={}\ndestination_messages={}\nsource_bytes={}\ndestination_bytes={}\nunmatched_messages={}\nfailed_messages={}\nauthoritative={}\n",
        snapshot_sha256(plan_snapshot),
        evidence.source_folders,
        evidence.destination_folders,
        evidence.source_messages,
        evidence.destination_messages,
        evidence.source_bytes,
        evidence.destination_bytes,
        evidence.unmatched_messages,
        evidence.failed_messages,
        evidence.authoritative,
    );
    snapshot_sha256(&canonical)
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

pub(crate) fn canonical_signed_proof_payload(value: &serde_json::Value) -> Result<String, String> {
    let mut unsigned = value.clone();
    unsigned
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?
        .remove("proof_signature");
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
