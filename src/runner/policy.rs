//! Which verification and evidence an engine run may claim for a plan.

use super::*;

/// Whether this plan carries the metadata needed for exact message-level
/// verification. A disabled capability is a review condition, not a failed
/// transfer.
pub(crate) fn message_verification_enabled(form: &crate::Form) -> bool {
    !form.profile.justfolders
        && !form.profile.addheader
        // imapsync owns --automap semantics. Until its preflight mapping is
        // captured and bound to the run, the verifier must not recreate that
        // decision from SPECIAL-USE/name heuristics after the transfer.
        && !form.profile.automap
        && form.profile.sync_internaldates
        && !form.profile.allowsizemismatch
}

pub(super) const MAX_BODY_HASH_BYTES_PER_MESSAGE: u64 = 64 * 1024 * 1024;

pub(super) const MAX_BODY_HASH_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

pub(crate) fn validate_body_hash_limits(form: &crate::Form) -> Result<(), String> {
    if !form.profile.body_hash_verification {
        return Ok(());
    }
    if form.engine() != crate::core::Engine::ImapSync {
        return Err(
            "body-hash verification is currently available only for encrypted imapsync runs".into(),
        );
    }
    let per_message = form.profile.body_hash_max_bytes;
    let total = form.profile.body_hash_max_total_bytes;
    if per_message == 0 || total == 0 {
        return Err(
            "body-hash verification requires non-zero per-message and total byte bounds".into(),
        );
    }
    if per_message > MAX_BODY_HASH_BYTES_PER_MESSAGE {
        return Err(format!(
            "body-hash per-message bound exceeds the {}-byte safety limit",
            MAX_BODY_HASH_BYTES_PER_MESSAGE
        ));
    }
    if total > MAX_BODY_HASH_TOTAL_BYTES || total < per_message {
        return Err(format!(
            "body-hash total bound must be at least the per-message bound and no more than {} bytes",
            MAX_BODY_HASH_TOTAL_BYTES
        ));
    }
    if !message_verification_enabled(form) {
        return Err(
            "body-hash verification requires the same immutable metadata-preservation plan as independent verification"
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn automap_blocks_live_certification(form: &crate::Form) -> bool {
    form.engine() == core::Engine::ImapSync && form.profile.automap
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalEvidenceSource {
    Independent,
    Engine,
    Unavailable,
}

/// Select the only evidence source allowed to determine a terminal state.
/// Live imapsync engine counters are telemetry when the plan cannot support
/// independent metadata reconciliation; they are never certification.
pub(crate) fn terminal_evidence_source(
    form: &crate::Form,
    independent_available: bool,
    engine_available: bool,
) -> TerminalEvidenceSource {
    if !form.dry_run && form.engine() == core::Engine::ImapSync {
        if message_verification_enabled(form) && independent_available {
            TerminalEvidenceSource::Independent
        } else {
            TerminalEvidenceSource::Unavailable
        }
    } else if independent_available {
        TerminalEvidenceSource::Independent
    } else if engine_available {
        TerminalEvidenceSource::Engine
    } else {
        TerminalEvidenceSource::Unavailable
    }
}
