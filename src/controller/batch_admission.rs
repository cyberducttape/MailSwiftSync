//! Batch admission and queue policy shared by GUI and headless callers.

use super::batch::BatchExecutionMode;
use crate::{Profile, bulk_import::BulkJob, core, effective_destination_tls, endpoint};
use std::collections::HashSet;

/// The durable batch configuration excludes free-form expert options. Those
/// options are revalidated from the current editable profile before launch;
/// the run snapshot records their digest instead of persisting raw text.
pub(crate) fn durable_batch_profile_config(profile: &Profile) -> Result<String, String> {
    let mut safe_profile = profile.clone();
    safe_profile.extra_options.clear();
    toml::to_string(&safe_profile)
        .map_err(|error| format!("Could not serialize batch plan: {error}"))
}

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

/// Validate and prepare the selected rows after durable admission facts have
/// been read. The returned forms are the exact copies that may be handed to
/// the worker pool; callers must not revalidate one representation and then
/// execute another.
pub(crate) fn prepare_selected_batch_jobs(
    source_jobs: &[BulkJob],
    selected_indices: &[usize],
    durable_admissions: &[Option<core::BatchAdmissionState>],
    mode: BatchExecutionMode,
    expected_credential_fingerprints: &[Option<String>],
) -> Result<Vec<BulkJob>, String> {
    if mode.is_live() {
        for &index in selected_indices {
            let job = source_jobs
                .get(index)
                .ok_or_else(|| format!("Batch queue row {} no longer exists.", index + 1))?;
            let admission = durable_admissions.get(index).and_then(Option::as_ref);
            let state = admission.map(|value| value.state.as_str());
            let preflight = admission.and_then(|value| value.preflight_plan.as_deref());
            if !matches!(
                state,
                Some(
                    "ready"
                        | "delta_required"
                        | "verification_difference"
                        | "failed"
                        | "attention"
                        | "cancelled"
                        | "completed"
                        | "verified"
                        | "verified_with_exceptions"
                )
            ) || preflight
                != Some(
                    crate::plan_identity::fingerprint_digest(&job.form.plan_fingerprint()).as_str(),
                )
            {
                return Err(format!(
                    "Mailbox {} is not ready for live execution. Re-run dry validation after reviewing its exact plan.",
                    index + 1
                ));
            }
        }
    }
    let mut jobs = selected_indices
        .iter()
        .map(|&index| {
            let mut job = source_jobs
                .get(index)
                .cloned()
                .ok_or_else(|| format!("Batch queue row {} no longer exists.", index + 1))?;
            job.form.dry_run = mode.is_preflight();
            Ok(job)
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (selected_index, job) in jobs.iter_mut().enumerate() {
        let queue_index = selected_indices[selected_index];
        let credential_load = if mode.is_live() {
            job.form.reload_configured_keyring_credentials()
        } else {
            job.form.load_configured_keyring_credentials()
        };
        if let Err(error) = credential_load {
            return Err(format!(
                "Could not load credentials for mailbox {} (queue row {}): {error}",
                selected_index + 1,
                queue_index + 1
            ));
        }
        if mode.is_live() {
            let current = job.form.credential_fingerprint();
            let expected = expected_credential_fingerprints
                .get(queue_index)
                .and_then(Option::as_deref);
            if expected != Some(current.as_str()) {
                return Err(format!(
                    "Mailbox {} credentials changed or were not retained from dry validation. Run a new dry validation before live execution.",
                    queue_index + 1
                ));
            }
        }
    }
    if let Some((selected_index, error)) = jobs
        .iter()
        .enumerate()
        .find_map(|(index, job)| job.form.validate().err().map(|error| (index, error)))
    {
        return Err(format!(
            "Mailbox {} is not ready for validation: {error}",
            selected_indices[selected_index] + 1
        ));
    }
    if mode.is_live()
        && jobs
            .iter()
            .any(|job| job.form.requires_insecure_transport_ack())
    {
        return Err("Live batch blocked: explicitly acknowledge that plain IMAP exposes credentials and mail in transit for every affected row.".into());
    }
    if mode.is_live()
        && let Some(error) = duplicate_destination(&jobs)?
    {
        return Err(error);
    }
    Ok(jobs)
}

pub(crate) struct PreparedBatchRun {
    pub(crate) selected_job_ids: Vec<String>,
    pub(crate) queue_checkpoints: Vec<Option<String>>,
    pub(crate) expected_plans: Vec<String>,
    pub(crate) batch_plan_fingerprints: Vec<String>,
    pub(crate) plan_snapshot: String,
    pub(crate) child_plans: Vec<core::BatchChildPlan>,
}

/// Materialize the exact durable inputs for an admitted batch. This is a
/// deterministic controller operation and deliberately has no egui or
/// process-launch responsibilities.
pub(crate) fn prepare_batch_run(
    jobs: &[BulkJob],
    selected_indices: &[usize],
    queue_job_ids: &[String],
    durable_admissions: &[Option<core::BatchAdmissionState>],
    mode: BatchExecutionMode,
) -> Result<PreparedBatchRun, String> {
    let selected_job_ids = selected_indices
        .iter()
        .map(|&index| {
            queue_job_ids
                .get(index)
                .cloned()
                .ok_or_else(|| format!("Batch queue row {} has no durable job ID.", index + 1))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let queue_checkpoints = selected_indices
        .iter()
        .zip(jobs.iter())
        .map(|(&index, job)| {
            if mode.is_live() && job.form.engine() == core::Engine::Dovecot {
                durable_admissions
                    .get(index)
                    .and_then(Option::as_ref)
                    .and_then(|admission| admission.checkpoint.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let batch_plan_fingerprints = jobs
        .iter()
        .map(|job| crate::plan_identity::fingerprint_digest(&job.form.plan_fingerprint()))
        .collect::<Vec<_>>();
    let expected_plans = if mode.is_live() {
        batch_plan_fingerprints.clone()
    } else {
        Vec::new()
    };
    let snapshots = jobs
        .iter()
        .zip(queue_checkpoints.iter())
        .map(|(job, checkpoint)| {
            job.form
                .plan_snapshot_with_checkpoint(checkpoint.as_deref())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let child_plans = jobs
        .iter()
        .zip(snapshots.iter())
        .map(|(job, plan_snapshot)| core::BatchChildPlan {
            engine: job.form.engine().label().to_owned(),
            plan_snapshot: plan_snapshot.clone(),
            engine_version: None,
        })
        .collect();
    Ok(PreparedBatchRun {
        selected_job_ids,
        queue_checkpoints,
        expected_plans,
        batch_plan_fingerprints,
        plan_snapshot: snapshots.join("\n--- batch mailbox plan ---\n"),
        child_plans,
    })
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

#[cfg(test)]
mod tests {
    use super::{BatchExecutionMode, prepare_selected_batch_jobs};
    use crate::{bulk_import::BulkJob, migration_plan::Form};

    #[test]
    fn selected_batch_preparation_fails_closed_without_durable_live_admission() {
        let jobs = [BulkJob {
            label: "mailbox".into(),
            form: Form::default(),
            state: "imported".into(),
        }];
        let error =
            prepare_selected_batch_jobs(&jobs, &[0], &[None], BatchExecutionMode::Live, &[])
                .err()
                .unwrap();
        assert!(error.contains("not ready for live execution"));
    }
}
