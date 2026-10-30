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
