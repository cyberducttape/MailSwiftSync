//! Build the durable provenance of one engine attempt from the launched
//! command. See `core::transfer_passes` for what the ledger keeps.

use super::batch_worker::{destination_endpoint, source_endpoint};
use crate::{
    Event,
    core::{self, TransferPassIntent},
    migration_plan::DovecotMigrationStrategy,
    runner::send_reliable_event,
};
use std::{path::PathBuf, sync::mpsc, time::Duration};

const PRIVATE_FILE: &str = "<private-runtime-file>";

/// Digest of the canonical source/destination mailbox pair. Stable across
/// runs, so successive passes of one mailbox share it.
pub(crate) fn mailbox_digest(form: &crate::Form) -> String {
    core::sha256_hex(
        format!(
            "mailbox-pair-v1\0{}\0{}\0{}\0{}",
            source_endpoint(form),
            form.profile.source_user.trim().to_lowercase(),
            destination_endpoint(form),
            form.profile.destination_user.trim().to_lowercase(),
        )
        .as_bytes(),
    )
}

fn pass_kind(form: &crate::Form) -> String {
    match form.engine() {
        core::Engine::Dovecot => match form.profile.dovecot_strategy {
            DovecotMigrationStrategy::InitialMirror => "dovecot_initial_mirror",
            DovecotMigrationStrategy::IncrementalMirror => "dovecot_incremental_mirror",
            DovecotMigrationStrategy::FinalPreservationPass => "dovecot_final_preservation",
            DovecotMigrationStrategy::DestinationAlreadyActive => {
                "dovecot_destination_already_active"
            }
        },
        // imapsync always walks the whole mailbox and skips messages that
        // are already present; initial versus incremental is the pass
        // sequence the ledger assigns.
        _ => "imapsync_sync",
    }
    .to_owned()
}

fn folder_scope(form: &crate::Form) -> String {
    if form.engine() == core::Engine::Dovecot {
        return "all_mailboxes;mapping=identity".to_owned();
    }
    format!(
        "{};mapping={}",
        if form.profile.justfolders {
            "folder_structure_only"
        } else {
            "all_selectable_folders"
        },
        if form.profile.automap {
            "automap"
        } else {
            "identity"
        }
    )
}

fn source_range(form: &crate::Form, checkpoint: Option<&str>) -> String {
    match (form.engine(), checkpoint.filter(|value| !value.is_empty())) {
        // Same digest the run's plan snapshot records for its resume point.
        (core::Engine::Dovecot, Some(checkpoint)) => format!(
            "resume_state:sha256={}",
            crate::plan_identity::snapshot_sha256(checkpoint)
        ),
        _ => "full_mailbox".to_owned(),
    }
}

/// The launched argv with secrets, private runtime file paths, and the
/// Dovecot resume-state token replaced by placeholders.
fn secret_free_command(
    form: &crate::Form,
    executable: &str,
    args: &[String],
    private_paths: &[PathBuf],
) -> Vec<String> {
    let private = private_paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    let dovecot = form.engine() == core::Engine::Dovecot;
    let mut command = vec![executable.to_owned()];
    let mut previous = "";
    for arg in args {
        let value = if dovecot && previous == "-s" {
            if arg.is_empty() {
                "<resume-state:initial>".to_owned()
            } else {
                "<resume-state>".to_owned()
            }
        } else if private.iter().any(|path| arg.contains(path.as_str())) {
            PRIVATE_FILE.to_owned()
        } else {
            crate::ui::redact_secrets(
                arg,
                [
                    form.source_password.as_str(),
                    form.destination_password.as_str(),
                ],
            )
        };
        command.push(value);
        previous = arg;
    }
    command
}

pub(crate) fn transfer_pass_intent(
    form: &crate::Form,
    executable: &str,
    args: &[String],
    private_paths: &[PathBuf],
    checkpoint: Option<&str>,
) -> TransferPassIntent {
    TransferPassIntent {
        mailbox_digest: mailbox_digest(form),
        pass_kind: pass_kind(form),
        engine: match form.engine() {
            core::Engine::Dovecot => "dovecot",
            _ => "imapsync",
        }
        .to_owned(),
        executable_identity: crate::plan_identity::executable_content_identity(executable),
        command: secret_free_command(form, executable, args, private_paths),
        folder_scope: folder_scope(form),
        source_range: source_range(form, checkpoint),
    }
}

/// Verification cursors as ledger rows: folder names become project-scoped
/// digests before anything leaves the process.
pub(crate) fn verified_folders(
    project_id: &str,
    cursors: &[core::FolderCursor],
) -> Vec<core::TransferPassFolder> {
    cursors
        .iter()
        .map(|cursor| core::TransferPassFolder {
            side: match cursor.side {
                core::StagedMessageSide::Source => core::PassSide::Source,
                core::StagedMessageSide::Destination => core::PassSide::Destination,
            },
            folder_digest: core::folder_digest(project_id, &cursor.mailbox),
            uidvalidity: cursor.snapshot.uidvalidity,
            uidnext: cursor.snapshot.uidnext,
            exists: cursor.snapshot.exists,
            verified_through_uid: cursor.last_uid,
            staged_messages: cursor.staged,
            complete: cursor.completed,
        })
        .collect()
}

const VERIFIED_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Durably attach verification results to the transfer attempt they
/// examined, waiting for the reducer's acknowledgement.
pub(crate) fn record_pass_verification(
    tx: &mpsc::SyncSender<Event>,
    run_id: &str,
    job_id: &str,
    attempt: u32,
    evidence: &core::MailboxEvidence,
    folders: Vec<core::TransferPassFolder>,
) -> Result<(), String> {
    let (reply, acknowledgement) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        Event::TransferPassVerified {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
            attempt,
            method: evidence.verification_method.as_str().to_owned(),
            outcome: evidence.verification_outcome().as_str().to_owned(),
            folders,
            reply,
        },
    )?;
    acknowledgement
        .recv_timeout(VERIFIED_ACK_TIMEOUT)
        .map_err(|_| "transfer-pass verification acknowledgement timed out".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> crate::Form {
        let mut form = crate::Form::default();
        form.profile.source_host = "imap.old.example".into();
        form.profile.source_user = "Alice@Old.Example".into();
        form.profile.destination_host = "imap.new.example".into();
        form.profile.destination_user = "alice@new.example".into();
        form.source_password = String::from("source-pass").into();
        form.destination_password = String::from("destination-pass").into();
        form
    }

    #[test]
    fn command_keeps_options_and_drops_secrets_paths_and_state() {
        let private = PathBuf::from("/run/user/1000/mailswiftsync/secret-abc");
        let args = vec![
            "--user1".into(),
            "Alice@Old.Example".into(),
            "--passfile1".into(),
            format!("{}/source.secret", private.display()),
            "--inline".into(),
            "contains source-pass here".into(),
        ];
        let intent = transfer_pass_intent(&form(), "/usr/bin/imapsync", &args, &[private], None);
        assert_eq!(
            intent.command,
            [
                "/usr/bin/imapsync",
                "--user1",
                "Alice@Old.Example",
                "--passfile1",
                PRIVATE_FILE,
                "--inline",
                "contains [REDACTED] here",
            ]
        );
        assert_eq!(intent.pass_kind, "imapsync_sync");
        assert_eq!(
            intent.folder_scope,
            "all_selectable_folders;mapping=identity"
        );
        assert_eq!(intent.source_range, "full_mailbox");
    }

    #[test]
    fn dovecot_resume_state_is_recorded_only_by_digest() {
        let mut form = form();
        form.profile.engine = core::Engine::Dovecot;
        form.profile.dovecot_strategy = DovecotMigrationStrategy::IncrementalMirror;
        let args = vec!["backup".into(), "-s".into(), "c3RhdGU=".into()];
        let intent = transfer_pass_intent(&form, "doveadm", &args, &[], Some("c3RhdGU=|context"));
        assert_eq!(
            intent.command,
            ["doveadm", "backup", "-s", "<resume-state>"]
        );
        assert_eq!(intent.pass_kind, "dovecot_incremental_mirror");
        assert_eq!(
            intent.source_range,
            format!(
                "resume_state:sha256={}",
                crate::plan_identity::snapshot_sha256("c3RhdGU=|context")
            )
        );
        let initial = transfer_pass_intent(
            &form,
            "doveadm",
            &["backup".into(), "-s".into(), String::new()],
            &[],
            None,
        );
        assert_eq!(initial.command[3], "<resume-state:initial>");
        assert_eq!(initial.source_range, "full_mailbox");
    }

    #[test]
    fn mailbox_digest_is_stable_across_spelling_and_distinct_per_pair() {
        let mut respelled = form();
        respelled.profile.source_user = "alice@old.example ".into();
        respelled.profile.source_host = "IMAP.OLD.EXAMPLE:993".into();
        assert_eq!(mailbox_digest(&form()), mailbox_digest(&respelled));
        let mut other = form();
        other.profile.destination_user = "bob@new.example".into();
        assert_ne!(mailbox_digest(&form()), mailbox_digest(&other));
    }
}
