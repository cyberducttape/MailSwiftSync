//! Shared validation for the protocol's nonzero 32-bit UID domain.

const MAX_IMAP_UID: u64 = u32::MAX as u64;

pub(super) fn validate_imap_uid(uid: u64, host: &str, mailbox: &str) -> Result<u64, String> {
    if uid == 0 || uid > MAX_IMAP_UID {
        return Err(format!(
            "{host}: folder {mailbox} returned UID {uid} outside the IMAP 1..={} domain",
            MAX_IMAP_UID
        ));
    }
    Ok(uid)
}
