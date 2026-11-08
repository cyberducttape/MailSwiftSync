//! Tamper-evident audit entries. Each entry's hash covers the previous
//! entry's hash and every field of its own, so editing, removing, or
//! reordering a stored entry breaks verification from that point on.

use sha2::Digest as _;

/// One audited decision.
#[derive(Clone, Debug)]
pub(super) struct AuditEntry<'a> {
    pub(super) at: i64,
    pub(super) request_id: &'a str,
    pub(super) actor: &'a str,
    pub(super) tenant: Option<&'a str>,
    pub(super) action: &'a str,
    pub(super) resource: &'a str,
    pub(super) allowed: bool,
    pub(super) detail: &'a str,
}

/// SHA-256 over the previous hash and length-prefixed fields, so no field
/// boundary can be shifted to produce the same digest.
pub(super) fn chain_hash(previous: &[u8; 32], entry: &AuditEntry<'_>) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"mailswiftsync-control-audit-v1");
    hasher.update(previous);
    let at = entry.at.to_string();
    let decision = if entry.allowed { "allow" } else { "deny" };
    for field in [
        Some(at.as_str()),
        Some(entry.request_id),
        Some(entry.actor),
        entry.tenant,
        Some(entry.action),
        Some(entry.resource),
        Some(decision),
        Some(entry.detail),
    ] {
        match field {
            None => hasher.update([0_u8]),
            Some(value) => {
                hasher.update([1_u8]);
                hasher.update((value.len() as u64).to_be_bytes());
                hasher.update(value.as_bytes());
            }
        }
    }
    hasher.finalize().into()
}

/// The last audit entry's sequence number and hash.
pub(super) type ChainHead = (i64, [u8; 32]);

/// An audit checkpoint: the chain head at a moment, to be signed and kept
/// outside the service (a ticket, WORM storage, a transparency log). The
/// hash chain alone cannot catch someone with write access to the database
/// rebuilding the whole chain; a published checkpoint can, because the
/// rebuilt chain no longer has the published hash at that sequence number.
pub(super) fn checkpoint(
    entries: u64,
    head: Option<ChainHead>,
) -> Result<serde_json::Value, String> {
    let (seq, hash) = head.ok_or("the audit log is empty; there is nothing to checkpoint")?;
    Ok(serde_json::json!({
        "format": crate::cli::CONTROL_AUDIT_CHECKPOINT_FORMAT,
        "format_version": 1,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "entries": entries,
        "seq": seq,
        "head_hash": crate::reports::integrity::hex_encode(&hash),
    }))
}

/// Check a checkpoint against the live log: its signature (pinned to
/// `public_key` when given), then that the entry it names still has the
/// hash it recorded. The caller verifies the chain itself.
pub(super) fn check_checkpoint(
    path: &std::path::Path,
    public_key: Option<&str>,
    hash_at: impl Fn(i64) -> Result<Option<Vec<u8>>, String>,
) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read the checkpoint: {error}"))?;
    let document: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("the checkpoint is not JSON: {error}"))?;
    if document["format"] != crate::cli::CONTROL_AUDIT_CHECKPOINT_FORMAT
        || document["format_version"] != 1
    {
        return Err("the file is not a control-plane audit checkpoint".into());
    }
    let signed = document.get("proof_signature").is_some();
    let trust = if signed {
        crate::reports::signing::verify_file(path, public_key)?;
        if public_key.is_some() {
            "signed by the trusted key"
        } else {
            "signature valid, signer not pinned (pass --public-key)"
        }
    } else if public_key.is_some() {
        return Err("the checkpoint is unsigned, but --public-key asks for a signature".into());
    } else {
        "unsigned"
    };
    let seq = document["seq"]
        .as_i64()
        .ok_or("the checkpoint has no sequence number")?;
    let recorded = crate::reports::integrity::hex_decode::<32>(
        document["head_hash"]
            .as_str()
            .ok_or("the checkpoint has no head hash")?,
    )?;
    match hash_at(seq)? {
        Some(hash) if hash == recorded => Ok(format!(
            "checkpoint at entry {seq} matches the log ({trust})"
        )),
        Some(_) => Err(format!(
            "audit log entry {seq} no longer has the checkpointed hash: the log was rewritten after the checkpoint"
        )),
        None => Err(format!(
            "audit log entry {seq} named by the checkpoint is missing: the log was truncated or replaced"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(tenant: Option<&'a str>, detail: &'a str) -> AuditEntry<'a> {
        AuditEntry {
            at: 1,
            request_id: "r",
            actor: "user:a",
            tenant,
            action: "x",
            resource: "y",
            allowed: true,
            detail,
        }
    }

    #[test]
    fn every_field_and_the_previous_hash_change_the_digest() {
        let base = chain_hash(&[0; 32], &entry(None, "d"));
        assert_ne!(base, chain_hash(&[1; 32], &entry(None, "d")));
        assert_ne!(base, chain_hash(&[0; 32], &entry(Some(""), "d")));
        assert_ne!(base, chain_hash(&[0; 32], &entry(None, "e")));
        let mut denied = entry(None, "d");
        denied.allowed = false;
        assert_ne!(base, chain_hash(&[0; 32], &denied));
    }
}
