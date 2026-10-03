use super::*;

pub(crate) const MAX_PERSISTED_PROFILE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TOTAL_PERSISTED_PROFILE_BYTES: usize = 256 * 1024 * 1024;

pub(crate) fn normalized_destination_identity(
    destination_mailbox: &str,
    config: Option<&str>,
) -> String {
    if let Some(config) = config
        && config.len() <= MAX_PERSISTED_PROFILE_BYTES
        && let Ok(value) = toml::from_str::<toml::Value>(config)
    {
        let host = value
            .get("destination_host")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|host| !host.is_empty());
        let user = value
            .get("destination_user")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|user| !user.is_empty());
        if let (Some(host), Some(user)) = (host, user) {
            let tls = value
                .get("destination_tls")
                .and_then(toml::Value::as_str)
                .unwrap_or("imaps");
            let port = value
                .get("destination_port")
                .and_then(toml::Value::as_str)
                .unwrap_or_default();
            return destination_identity_from_parts(destination_mailbox, user, host, tls, port);
        }
    }
    crate::endpoint::mailbox_identity(destination_mailbox)
}

/// `normalized_destination_identity` for a plan whose destination fields
/// are already known, so a bulk insert need not re-parse every plan.
pub fn destination_identity_from_parts(
    destination_mailbox: &str,
    user: &str,
    host: &str,
    tls: &str,
    port: &str,
) -> String {
    let (user, host) = (user.trim(), host.trim());
    if !user.is_empty()
        && !host.is_empty()
        && let Ok(identity) =
            crate::endpoint::canonical_destination_identity(user, host, tls, port.trim())
    {
        return identity;
    }
    crate::endpoint::mailbox_identity(destination_mailbox)
}

/// Conservative cross-project lock identity. Exact durable identities retain
/// mailbox spelling for providers with unknown rules; active-run exclusion
/// additionally case-folds it so uncertain aliases cannot write in parallel.
pub(crate) fn normalized_destination_lock_identity(
    destination_mailbox: &str,
    config: Option<&str>,
) -> String {
    if let Some(config) = config
        && config.len() <= MAX_PERSISTED_PROFILE_BYTES
        && let Ok(value) = toml::from_str::<toml::Value>(config)
    {
        let host = value
            .get("destination_host")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|host| !host.is_empty());
        let user = value
            .get("destination_user")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|user| !user.is_empty());
        if let (Some(host), Some(user)) = (host, user) {
            let tls = value
                .get("destination_tls")
                .and_then(toml::Value::as_str)
                .unwrap_or("imaps");
            let port = value
                .get("destination_port")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if let Ok(identity) = crate::endpoint::casefolded_destination_identity(
                user,
                host,
                crate::effective_destination_tls(tls),
                port,
            ) {
                return identity;
            }
        }
    }
    crate::endpoint::mailbox_identity(destination_mailbox)
}

const MAX_DOVECOT_CHECKPOINT_BYTES: usize = 4096;
const DOVECOT_CHECKPOINT_CONTEXT_BYTES: usize = 64;

/// A checkpoint context binds an opaque dsync state token to the mailbox
/// identity inventory observed after the run. The engine still receives only
/// the state token; the context is control-plane metadata used to fail closed
/// before resuming against a recreated mailbox.
pub(crate) fn encode_dovecot_checkpoint(state: &str, context: &str) -> Option<String> {
    if !valid_dovecot_checkpoint_raw(state)
        || context.len() != DOVECOT_CHECKPOINT_CONTEXT_BYTES
        || !context.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let value = format!("{state}.{context}");
    (value.len() <= MAX_DOVECOT_CHECKPOINT_BYTES).then_some(value)
}

pub(crate) fn dovecot_checkpoint_state(value: &str) -> &str {
    value.split_once('.').map_or(value, |(state, _)| state)
}

pub(crate) fn dovecot_checkpoint_context(value: &str) -> Option<&str> {
    let (state, context) = value.split_once('.')?;
    (valid_dovecot_checkpoint_raw(state)
        && context.len() == DOVECOT_CHECKPOINT_CONTEXT_BYTES
        && context.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(context)
}
pub(crate) fn attention_reason_for(mailbox_state: &str, detail: &str) -> Option<AttentionReason> {
    if mailbox_state == "verification_difference" {
        return Some(AttentionReason::VerificationDifference);
    }
    if !matches!(mailbox_state, "attention" | "failed" | "cancelled") {
        return None;
    }
    if let Some(reason) = detail
        .strip_prefix("[attention_reason=")
        .and_then(|value| value.split_once(']'))
        .and_then(|(value, _)| AttentionReason::parse(value))
    {
        return Some(reason);
    }
    let detail = detail.to_ascii_lowercase();
    if detail.contains("identity") || detail.contains("ownership") || detail.contains("unverified")
    {
        return Some(AttentionReason::ProcessIdentityUnverified);
    }
    if detail.contains("restart") || detail.contains("interrupt") {
        return Some(AttentionReason::Interrupted);
    }
    if detail.contains("verification") || detail.contains("evidence") {
        return Some(AttentionReason::VerificationIncomplete);
    }
    if detail.contains("auth") || detail.contains("credential") || detail.contains("password") {
        return Some(AttentionReason::AuthenticationFailed);
    }
    if detail.contains("rate") || detail.contains("quota") || detail.contains("capacity") {
        return Some(AttentionReason::CapacityLimited);
    }
    if detail.contains("policy") {
        return Some(AttentionReason::PolicyBlocked);
    }
    if detail.contains("config") || detail.contains("invalid") {
        return Some(AttentionReason::ConfigurationInvalid);
    }
    if detail.contains("network") || detail.contains("timeout") || detail.contains("connection") {
        return Some(AttentionReason::TransportFailed);
    }
    if mailbox_state == "cancelled" {
        return Some(AttentionReason::Interrupted);
    }
    if mailbox_state == "failed" {
        return Some(AttentionReason::Unknown);
    }
    Some(AttentionReason::Unknown)
}

pub(crate) fn valid_dovecot_checkpoint(value: &str) -> bool {
    if let Some((state, context)) = value.split_once('.') {
        return encode_dovecot_checkpoint(state, context).as_deref() == Some(value);
    }
    valid_dovecot_checkpoint_raw(value)
}

fn valid_dovecot_checkpoint_raw(value: &str) -> bool {
    if value != value.trim()
        || value.len() < 8
        || value.len() > MAX_DOVECOT_CHECKPOINT_BYTES
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "success" | "successful" | "completed"
        )
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'+' | b'/' | b'='))
    {
        return false;
    }
    let padding_start = value.find('=');
    if let Some(index) = padding_start {
        if !value.len().is_multiple_of(4)
            || value[index..].len() > 2
            || value[index..].bytes().any(|byte| byte != b'=')
        {
            return false;
        }
    } else if value.len() % 4 == 1 {
        return false;
    }
    let mut padded = value.to_owned();
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    let Ok(decoded) = BASE64_STANDARD.decode(padded) else {
        return false;
    };

    // This mirrors Dovecot's documented dsync state format rather than
    // treating every syntactically valid Base64 string as a checkpoint:
    // v0's empty state is four zero bytes; v1 is a four-byte header followed
    // by fixed-size mailbox records and a little-endian CRC32.
    if decoded == [0, 0, 0, 0] {
        return true;
    }
    const HEADER_SIZE: usize = 4;
    const CRC_SIZE: usize = 4;
    const MAILBOX_STATE_SIZE: usize = 44;
    if decoded.len() < HEADER_SIZE + CRC_SIZE
        || decoded[..HEADER_SIZE] != [1, 0, 0, 0]
        || !(decoded.len() - HEADER_SIZE - CRC_SIZE).is_multiple_of(MAILBOX_STATE_SIZE)
    {
        return false;
    }
    let checksum_offset = decoded.len() - CRC_SIZE;
    let expected = u32::from_le_bytes([
        decoded[checksum_offset],
        decoded[checksum_offset + 1],
        decoded[checksum_offset + 2],
        decoded[checksum_offset + 3],
    ]);
    let mut hasher = Crc32Hasher::new();
    hasher.update(&decoded[..checksum_offset]);
    hasher.finalize() == expected
}

pub(crate) fn phase_rank(phase: Phase) -> u8 {
    match phase {
        Phase::Discovery => 0,
        Phase::Preflight => 1,
        Phase::Pilot => 2,
        Phase::Seed => 3,
        Phase::CatchUp => 4,
        Phase::FinalDelta => 5,
        Phase::Verification => 6,
        Phase::Complete => 7,
        Phase::Attention => 255,
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::{
        dovecot_checkpoint_context, dovecot_checkpoint_state, encode_dovecot_checkpoint,
        valid_dovecot_checkpoint,
    };

    const STATE: &str = "AQAAAHm4+Jk=";
    const CONTEXT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn checkpoint_context_round_trips_without_changing_engine_state() {
        let encoded = encode_dovecot_checkpoint(STATE, CONTEXT).unwrap();
        assert!(valid_dovecot_checkpoint(&encoded));
        assert_eq!(dovecot_checkpoint_state(&encoded), STATE);
        assert_eq!(dovecot_checkpoint_context(&encoded), Some(CONTEXT));
    }

    #[test]
    fn malformed_checkpoint_context_is_rejected() {
        assert!(!valid_dovecot_checkpoint(&format!("{STATE}.not-a-digest")));
        assert_eq!(dovecot_checkpoint_context(STATE), None);
    }
}
