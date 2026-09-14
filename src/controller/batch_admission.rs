//! Batch admission and queue policy shared by GUI and headless callers.

use super::batch::BatchExecutionMode;
use super::batch::BulkRetryScope;
use super::run::{ActiveRunContext, RunKind};
use crate::{Profile, bulk_import::BulkJob, core, effective_destination_tls, endpoint};
use std::collections::HashSet;

pub(crate) struct BatchProjectIdentity {
    pub(crate) name: String,
    pub(crate) source_endpoint: String,
    pub(crate) destination_endpoint: String,
}

pub(crate) struct BatchLaunchAdmission {
    pub(crate) project_id: String,
    pub(crate) job_ids: Vec<String>,
    pub(crate) selected_indices: Vec<usize>,
    pub(crate) jobs: Vec<BulkJob>,
    pub(crate) prepared: PreparedBatchRun,
    pub(crate) active_run: ActiveRunContext,
}

pub(crate) struct BatchLaunchRequest<'a> {
    pub(crate) store: &'a core::StateStore,
    pub(crate) requested_project_id: Option<&'a str>,
    pub(crate) source_jobs: &'a [BulkJob],
    pub(crate) queue_job_ids: &'a [String],
    pub(crate) selected_ids: &'a HashSet<String>,
    pub(crate) retry_scope: BulkRetryScope,
    pub(crate) mode: BatchExecutionMode,
    pub(crate) fallback_profile: &'a Profile,
    pub(crate) expected_credential_fingerprints: &'a [Option<String>],
    pub(crate) run_id: &'a str,
}

/// Perform the complete durable batch admission sequence before the UI owns
/// any worker or process state. This is the shared controller boundary for
/// GUI and headless batch launches.
pub(crate) fn admit_batch_launch(
    request: BatchLaunchRequest<'_>,
) -> Result<BatchLaunchAdmission, String> {
    let BatchLaunchRequest {
        store,
        requested_project_id,
        source_jobs,
        queue_job_ids,
        selected_ids,
        retry_scope,
        mode,
        fallback_profile,
        expected_credential_fingerprints,
        run_id,
    } = request;
    let durable_admissions = if let Some(project_id) =
        requested_project_id.filter(|_| queue_job_ids.len() == source_jobs.len())
    {
        store
            .batch_admission_states(project_id, queue_job_ids)
            .map_err(|error| {
                format!(
                    "Could not read durable batch admission state; batch was not started: {error}"
                )
            })?
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>()
    } else {
        vec![None; source_jobs.len()]
    };
    let durable_states = durable_admissions
        .iter()
        .map(|admission| admission.as_ref().map(|value| value.state.clone()))
        .collect::<Vec<_>>();
    let selected_indices = super::batch::selected_batch_indices(
        source_jobs.len(),
        queue_job_ids,
        &durable_states,
        selected_ids,
        retry_scope,
    );
    if selected_indices.is_empty() {
        return Err(format!(
            "No mailboxes match the selected live retry scope: {}.",
            retry_scope.label()
        ));
    }
    let jobs = prepare_selected_batch_jobs(
        source_jobs,
        &selected_indices,
        &durable_admissions,
        mode,
        expected_credential_fingerprints,
    )?;
    let concurrency = fallback_profile.batch_concurrency.clamp(1, 16);
    validate_batch_throttle(fallback_profile, concurrency)?;
    let mailboxes = jobs
        .iter()
        .map(|job| {
            let config = durable_batch_profile_config(&job.form.profile)?;
            Ok((
                job.form.profile.source_user.clone(),
                job.form.profile.destination_user.clone(),
                config,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let identity = batch_project_identity(&jobs, fallback_profile);
    let (project_id, job_ids) = prepare_batch_project(
        store,
        requested_project_id,
        &mailboxes,
        &identity.name,
        &identity.source_endpoint,
        &identity.destination_endpoint,
    )?;
    let prepared = prepare_batch_run(
        &jobs,
        &selected_indices,
        &job_ids,
        &durable_admissions,
        mode,
    )?;
    let active_run = admit_batch_run(
        store,
        &project_id,
        run_id,
        mode,
        fallback_profile.engine,
        prepared.clone(),
    )?;
    Ok(BatchLaunchAdmission {
        project_id,
        job_ids,
        selected_indices,
        jobs,
        prepared,
        active_run,
    })
}

/// Derive durable batch-project metadata from the admitted queue. Keeping
/// this together with admission ensures GUI and headless callers use the same
/// naming and endpoint contract.
pub(crate) fn batch_project_identity(
    jobs: &[BulkJob],
    fallback_profile: &Profile,
) -> BatchProjectIdentity {
    let profile = jobs
        .first()
        .map(|job| &job.form.profile)
        .unwrap_or(fallback_profile);
    BatchProjectIdentity {
        name: super::batch::suggested_batch_project_name(profile),
        source_endpoint: profile.source_host.trim().to_owned(),
        destination_endpoint: profile.destination_host.trim().to_owned(),
    }
}

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

#[derive(Clone)]
pub(crate) struct PreparedBatchRun {
    pub(crate) selected_job_ids: Vec<String>,
    pub(crate) queue_checkpoints: Vec<Option<String>>,
    pub(crate) expected_plans: Vec<String>,
    pub(crate) batch_plan_fingerprints: Vec<String>,
    pub(crate) plan_snapshot: String,
    pub(crate) child_plans: Vec<core::BatchChildPlan>,
}

/// Persist the admitted parent/child run set and return the ownership context
/// used by the batch worker and event reducer. Durable admission and the
/// in-memory ownership map are created together so callers cannot launch a
/// worker with a context that does not describe committed child runs.
pub(crate) fn admit_batch_run(
    store: &core::StateStore,
    project_id: &str,
    run_id: &str,
    mode: BatchExecutionMode,
    engine: core::Engine,
    prepared: PreparedBatchRun,
) -> Result<ActiveRunContext, String> {
    let child_run_ids = store
        .begin_batch_run_with_children(
            project_id,
            &prepared.selected_job_ids,
            run_id,
            if mode.is_live() {
                "batch migration"
            } else {
                "batch validation"
            },
            &prepared.expected_plans,
            &prepared.plan_snapshot,
            &prepared.child_plans,
        )
        .map_err(|error| format!("Could not start durable batch run: {error}"))?;
    Ok(ActiveRunContext {
        run_id: run_id.to_owned(),
        project_id: project_id.to_owned(),
        job_id: None,
        batch_job_ids: prepared.selected_job_ids,
        batch_child_indices: child_run_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.clone(), index))
            .collect(),
        batch_child_run_ids: child_run_ids,
        batch_plan_fingerprints: prepared.batch_plan_fingerprints,
        kind: RunKind::Batch,
        dry_run: mode.is_preflight(),
        engine,
        plan_fingerprint: String::new(),
        credential_fingerprint: String::new(),
    })
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
    use super::{
        BatchExecutionMode, BatchLaunchRequest, admit_batch_launch, batch_project_identity,
        prepare_selected_batch_jobs,
    };
    use crate::{bulk_import::BulkJob, migration_plan::Form};
    use std::collections::HashSet;

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

    #[test]
    fn batch_project_identity_uses_queue_profile_and_safe_fallback() {
        let mut form = Form::default();
        form.profile.source_host = " imap.source.example ".into();
        form.profile.destination_host = "imap.destination.example".into();
        form.profile.name = "Customer cutover".into();
        let jobs = [BulkJob {
            label: "mailbox".into(),
            form: form.clone(),
            state: "imported".into(),
        }];
        let identity = batch_project_identity(&jobs, &Form::default().profile);
        assert_eq!(identity.name, "Customer cutover");
        assert_eq!(identity.source_endpoint, "imap.source.example");
        assert_eq!(identity.destination_endpoint, "imap.destination.example");

        let fallback = batch_project_identity(&[], &form.profile);
        assert_eq!(fallback.name, "Customer cutover");
        assert_eq!(fallback.source_endpoint, "imap.source.example");
    }

    #[test]
    fn batch_launch_admission_rejects_an_empty_queue_before_persistence() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let profile = Form::default().profile;
        let error = admit_batch_launch(BatchLaunchRequest {
            store: &store,
            requested_project_id: None,
            source_jobs: &[],
            queue_job_ids: &[],
            selected_ids: &HashSet::new(),
            retry_scope: super::BulkRetryScope::All,
            mode: BatchExecutionMode::Preflight,
            fallback_profile: &profile,
            expected_credential_fingerprints: &[],
            run_id: "run-empty",
        })
        .err()
        .expect("empty queue must be rejected");
        assert!(error.contains("No mailboxes match"));
        assert!(store.latest_project().unwrap().is_none());
    }
}
