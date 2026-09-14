//! Batch admission and queue policy shared by GUI and headless callers.

use crate::{Profile, bulk_import::BulkJob, core, effective_destination_tls, endpoint};
use std::collections::HashSet;

pub(crate) fn selection_value(
    jobs: &[BulkJob],
    selected_ids: &HashSet<String>,
    job_ids: &[String],
) -> serde_json::Value {
    let rows = jobs
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            selected_ids.is_empty()
                || job_ids
                    .get(*index)
                    .is_some_and(|job_id| selected_ids.contains(job_id))
        })
        .map(|(_, job)| {
            serde_json::json!({
                "label": job.label,
                "source_host": job.form.profile.source_host,
                "source_user": job.form.profile.source_user,
                "destination_host": job.form.profile.destination_host,
                "destination_user": job.form.profile.destination_user,
                "state": crate::ui::display_state_key(&job.state),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "format": "mailswiftsync-batch-selection",
        "version": 1,
        "selected_rows": rows,
        "note": "This handoff intentionally excludes passwords, keyring references, extra options, and engine credentials. It is a review/retry scope, not an executable migration plan."
    })
}

pub(crate) fn canonical_destination_identity(profile: &Profile) -> Result<String, String> {
    endpoint::canonical_destination_identity(
        &profile.destination_user,
        &profile.destination_host,
        effective_destination_tls(&profile.destination_tls),
        &profile.destination_port,
    )
    .map_err(|error| format!("Invalid destination endpoint: {error}"))
}

/// Validate shared batch throughput constraints before any durable run is
/// created. This policy belongs to admission rather than the egui dispatcher
/// so GUI and headless callers cannot diverge.
pub(crate) fn validate_batch_throttle(profile: &Profile, concurrency: usize) -> Result<(), String> {
    let workers = concurrency.max(1);
    if profile.max_messages_per_second > 0 && profile.max_messages_per_second < workers as u32 {
        return Err(format!(
            "Messages/second target must be at least the batch concurrency ({workers}), or reduce concurrency."
        ));
    }
    if profile.max_bytes_per_second > 0 && profile.max_bytes_per_second < workers as u64 {
        return Err(format!(
            "Bytes/second target must be at least the batch concurrency ({workers}), or reduce concurrency."
        ));
    }
    Ok(())
}

/// Verify that an editable single-mailbox profile still names the durable
/// project and mailbox it is about to execute. Keeping this beside batch
/// admission prevents GUI and headless callers from inventing separate
/// identity rules.
pub(crate) fn durable_single_identity_matches(
    project: &core::Project,
    mailbox: &core::MailboxJob,
    profile: &Profile,
) -> bool {
    project.source_endpoint == profile.source_host
        && project.destination_endpoint == profile.destination_host
        && mailbox.source_mailbox == profile.source_user
        && mailbox.destination_mailbox == profile.destination_user
}

pub(crate) fn duplicate_destination(jobs: &[BulkJob]) -> Result<Option<String>, String> {
    let mut destinations = HashSet::new();
    for (index, job) in jobs.iter().enumerate() {
        let key = canonical_destination_identity(&job.form.profile)?;
        if !destinations.insert(key) {
            return Ok(Some(format!(
                "Mailbox {} targets a destination mailbox already used by another batch row; concurrent writes to one mailbox are blocked.",
                index + 1
            )));
        }
    }
    Ok(None)
}

pub(crate) fn matches_queue(
    stored: &[core::MailboxJob],
    desired: &[(String, String, String)],
) -> bool {
    stored.len() == desired.len()
        && stored.iter().zip(desired.iter()).all(
            |(stored, (source_mailbox, destination_mailbox, config))| {
                stored.source_mailbox == *source_mailbox
                    && stored.destination_mailbox == *destination_mailbox
                    && stored.config.as_deref() == Some(config.as_str())
            },
        )
}

/// Reuse a durable batch only when its mailbox/configuration rows exactly
/// match the admitted queue; otherwise create a new project atomically. This
/// keeps SQLite project-selection policy out of the egui start dispatcher.
pub(crate) fn prepare_batch_project(
    store: &core::StateStore,
    requested_project_id: Option<&str>,
    mailboxes: &[(String, String, String)],
    project_name: &str,
    source_endpoint: &str,
    destination_endpoint: &str,
) -> Result<(String, Vec<String>), String> {
    let reusable_project = if let Some(project_id) = requested_project_id {
        match store.mailboxes(project_id) {
            Ok(stored) if matches_queue(&stored, mailboxes) => Some(project_id.to_owned()),
            Ok(_) => None,
            Err(error) => {
                return Err(format!(
                    "Could not inspect the existing durable batch; no new batch was created: {error}"
                ));
            }
        }
    } else {
        None
    };
    if let Some(project_id) = reusable_project {
        let job_ids = store
            .mailboxes(&project_id)
            .map_err(|error| format!("Could not read the reusable durable batch: {error}"))?
            .into_iter()
            .map(|job| job.id)
            .collect();
        return Ok((project_id, job_ids));
    }
    let (project, job_ids) = store
        .create_project_with_mailbox_configs(
            project_name,
            source_endpoint,
            destination_endpoint,
            mailboxes,
        )
        .map_err(|error| format!("Could not create durable batch: {error}"))?;
    Ok((project.id, job_ids))
}

pub(crate) fn apply_keyring_id(jobs: &mut [BulkJob], id: &str, source: bool) -> usize {
    let mut applied = 0;
    for job in jobs {
        let password_empty = if source {
            job.form.source_password.is_empty()
        } else {
            job.form.destination_password.is_empty()
        };
        let credential_id = if source {
            &mut job.form.profile.source_credential_id
        } else {
            &mut job.form.profile.destination_credential_id
        };
        if password_empty && credential_id.trim().is_empty() {
            *credential_id = id.to_owned();
            applied += 1;
        }
    }
    applied
}
