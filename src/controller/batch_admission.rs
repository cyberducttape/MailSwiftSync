//! Batch admission and queue policy shared by GUI and headless callers.

use super::batch::BatchExecutionMode;
use super::batch::BulkRetryScope;
use super::run::{ActiveRunContext, RunKind};
use crate::core::{MAX_PERSISTED_PROFILE_BYTES, MAX_TOTAL_PERSISTED_PROFILE_BYTES};
use crate::{Profile, bulk_import::BulkJob, core, effective_destination_tls, endpoint};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub(crate) struct BatchProjectIdentity {
    pub(crate) name: String,
    pub(crate) source_endpoint: String,
    pub(crate) destination_endpoint: String,
}

pub(crate) struct BatchLaunchAdmission {
    pub(crate) project_id: String,
    pub(crate) job_ids: Vec<String>,
    pub(crate) selected_jobs: Vec<SelectedBatchJob>,
    pub(crate) prepared: PreparedBatchRun,
    pub(crate) active_run: ActiveRunContext,
}

/// One execution-scoped row with all identity domains joined explicitly.
/// `queue_index` is only an in-memory position; `durable_job_id` is the
/// identity persisted in the existing batch project and used for the run.
#[derive(Clone)]
pub(crate) struct SelectedBatchJob {
    pub(crate) queue_index: usize,
    pub(crate) durable_job_id: String,
    pub(crate) job: BulkJob,
    pub(crate) admission: Option<core::BatchAdmissionState>,
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

/// Decode a persisted batch row without ever substituting a default plan.
/// Missing or corrupt durable configuration is state corruption, not a
/// request for a new migration profile.
pub(crate) fn decode_persisted_batch_profile(
    config: Option<&str>,
    job_id: &str,
) -> Result<Profile, String> {
    let config =
        config.ok_or_else(|| format!("Saved batch mailbox {job_id} has no migration plan"))?;
    if config.len() > MAX_PERSISTED_PROFILE_BYTES {
        return Err(format!(
            "Saved batch mailbox {job_id} migration plan exceeds the {MAX_PERSISTED_PROFILE_BYTES}-byte limit"
        ));
    }
    toml::from_str(config)
        .map_err(|error| format!("Saved batch mailbox {job_id} is corrupt: {error}"))
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
    if !selected_ids.is_empty() && queue_job_ids.len() != source_jobs.len() {
        return Err(
            "Explicit batch selection cannot be resolved because the queue has no complete durable identity; create or restore the batch project first."
                .into(),
        );
    }
    let selected_jobs = prepare_selected_batch_jobs(
        source_jobs,
        &selected_indices,
        &durable_admissions,
        mode,
        expected_credential_fingerprints,
    )?;
    let concurrency = fallback_profile.batch_concurrency.clamp(1, 16);
    validate_batch_throttle(fallback_profile, concurrency)?;
    // Durable project identity belongs to the complete queue, not to the
    // selected retry subset.  The selected indices are positions in
    // `source_jobs`; creating a subset project here would produce a shorter
    // `job_ids` vector and make those two index spaces incompatible.
    let mailboxes = source_jobs
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
    let identity = batch_project_identity(source_jobs, fallback_profile);
    let (project_id, job_ids) = prepare_batch_project(
        store,
        requested_project_id,
        &mailboxes,
        &identity.name,
        &identity.source_endpoint,
        &identity.destination_endpoint,
    )?;
    if job_ids.len() != source_jobs.len() {
        return Err(
            "Durable batch project does not contain the complete admitted queue; refusing ambiguous execution."
                .into(),
        );
    }
    if requested_project_id.is_some_and(|requested| requested != project_id) {
        return Err(
            "Durable batch project identity changed during admission; refusing execution.".into(),
        );
    }
    if selected_jobs.is_empty() || selected_jobs.len() != selected_indices.len() {
        return Err(
            "Selected batch rows could not be resolved in the admitted project; refusing execution."
                .into(),
        );
    }
    let durable_job_ids = job_ids.iter().collect::<HashSet<_>>();
    let mut selected_jobs = selected_jobs;
    for selected in &mut selected_jobs {
        selected.durable_job_id = job_ids.get(selected.queue_index).cloned().ok_or_else(|| {
            format!(
                "Batch queue row {} has no durable job ID in the admitted project.",
                selected.queue_index + 1
            )
        })?;
        if !durable_job_ids.contains(&selected.durable_job_id) {
            return Err(
                "Selected batch row does not belong to the admitted durable project; refusing execution."
                    .into(),
            );
        }
    }
    let prepared = prepare_batch_run(&selected_jobs, mode)?;
    let active_run = admit_batch_run(store, &project_id, run_id, mode, prepared.clone())?;
    Ok(BatchLaunchAdmission {
        project_id,
        job_ids,
        selected_jobs,
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
    let queue_profile = jobs
        .first()
        .map(|job| &job.form.profile)
        .unwrap_or(fallback_profile);
    let configured_fallback = fallback_profile.name.trim();
    let profile = if !configured_fallback.is_empty()
        && !matches!(
            configured_fallback,
            "New migration" | "Batch migration" | "Batch validation"
        )
        && (queue_profile.name.trim().is_empty()
            || matches!(
                queue_profile.name.trim(),
                "New migration" | "Batch migration" | "Batch validation"
            )) {
        fallback_profile
    } else {
        queue_profile
    };
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

#[cfg(test)]
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
    if let Some(project_id) = requested_project_id {
        match store.mailbox_queue_matches(project_id, mailboxes) {
            Ok(true) => {
                let job_ids = store
                    .mailbox_ids(project_id)
                    .map_err(|error| format!("Could not read durable batch IDs: {error}"))?;
                return Ok((project_id.to_owned(), job_ids));
            }
            Ok(false) => {
                return Err(
                    "The existing durable batch no longer matches the admitted queue; refusing to create a replacement project. Re-import the queue as a new batch before retrying.".into(),
                );
            }
            Err(error) => {
                return Err(format!(
                    "Could not inspect the existing durable batch; no new batch was created: {error}"
                ));
            }
        }
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
) -> Result<Vec<SelectedBatchJob>, String> {
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
    let mut selected_jobs = selected_indices
        .iter()
        .map(|&index| {
            let mut job = source_jobs
                .get(index)
                .cloned()
                .ok_or_else(|| format!("Batch queue row {} no longer exists.", index + 1))?;
            job.form.dry_run = mode.is_preflight();
            Ok(SelectedBatchJob {
                queue_index: index,
                durable_job_id: String::new(),
                job,
                admission: durable_admissions.get(index).and_then(Clone::clone),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    for selected in &mut selected_jobs {
        let queue_index = selected.queue_index;
        let credential_load = if mode.is_live() {
            // Do not mint queue-wide OAuth access tokens here. A selected
            // mailbox may wait behind many workers; its live worker refreshes
            // immediately before authentication and engine launch.
            selected
                .job
                .form
                .load_static_configured_keyring_credentials()
        } else {
            selected.job.form.load_configured_keyring_credentials()
        };
        if let Err(error) = credential_load {
            return Err(format!(
                "Could not load credentials for queue row {} (durable mailbox {}): {error}",
                queue_index + 1,
                selected.durable_job_id
            ));
        }
        if mode.is_live() {
            let current = selected.job.form.credential_binding_fingerprint();
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
    if let Some(selected) = selected_jobs.iter().find_map(|selected| {
        selected
            .job
            .form
            .validate()
            .err()
            .map(|error| (selected, error))
    }) {
        let (selected, error) = selected;
        return Err(format!(
            "Mailbox {} is not ready for validation: {error}",
            selected.queue_index + 1
        ));
    }
    if mode.is_live()
        && selected_jobs
            .iter()
            .any(|selected| selected.job.form.requires_insecure_transport_ack())
    {
        return Err("Live batch blocked: explicitly acknowledge that plain IMAP exposes credentials and mail in transit for every affected row.".into());
    }
    if mode.is_live()
        && let Some(error) = duplicate_destination(
            &selected_jobs
                .iter()
                .map(|selected| selected.job.clone())
                .collect::<Vec<_>>(),
        )?
    {
        return Err(error);
    }
    Ok(selected_jobs)
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
        plan_fingerprint: String::new(),
        credential_fingerprint: String::new(),
    })
}

/// Materialize the exact durable inputs for an admitted batch. This is a
/// deterministic controller operation and deliberately has no egui or
/// process-launch responsibilities.
pub(crate) fn prepare_batch_run(
    selected_jobs: &[SelectedBatchJob],
    mode: BatchExecutionMode,
) -> Result<PreparedBatchRun, String> {
    if selected_jobs.is_empty() {
        return Err(
            "Batch admission resolved zero selected durable jobs; refusing to start.".into(),
        );
    }
    let selected_job_ids = selected_jobs
        .iter()
        .map(|selected| selected.durable_job_id.clone())
        .collect::<Vec<_>>();
    let queue_checkpoints = selected_jobs
        .iter()
        .map(|selected| {
            if mode.is_live() && selected.job.form.engine() == core::Engine::Dovecot {
                selected
                    .admission
                    .as_ref()
                    .and_then(|admission| admission.checkpoint.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let batch_plan_fingerprints = selected_jobs
        .iter()
        .map(|selected| {
            crate::plan_identity::fingerprint_digest(&selected.job.form.plan_fingerprint())
        })
        .collect::<Vec<_>>();
    let expected_plans = if mode.is_live() {
        batch_plan_fingerprints.clone()
    } else {
        Vec::new()
    };
    let mut batch_digest = Sha256::new();
    let mut child_plans = Vec::with_capacity(selected_jobs.len());
    let mut total_snapshot_bytes = 0usize;
    for (selected, checkpoint) in selected_jobs.iter().zip(queue_checkpoints.iter()) {
        let plan_snapshot = selected
            .job
            .form
            .plan_snapshot_with_checkpoint(checkpoint.as_deref())?;
        total_snapshot_bytes = total_snapshot_bytes
            .checked_add(plan_snapshot.len())
            .ok_or_else(|| "Batch plan snapshots exceed the aggregate size budget".to_owned())?;
        if total_snapshot_bytes > MAX_TOTAL_PERSISTED_PROFILE_BYTES {
            return Err(format!(
                "Batch plan snapshots exceed the aggregate {MAX_TOTAL_PERSISTED_PROFILE_BYTES}-byte budget"
            ));
        }
        batch_digest.update((plan_snapshot.len() as u64).to_le_bytes());
        batch_digest.update(plan_snapshot.as_bytes());
        child_plans.push(core::BatchChildPlan {
            engine: selected.job.form.engine().label().to_owned(),
            plan_snapshot,
            engine_version: None,
        });
    }
    let batch_plan_digest = batch_digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let plan_snapshot = format!(
        "batch_plan_count={}\nbatch_plan_sha256={batch_plan_digest}\n",
        child_plans.len()
    );
    Ok(PreparedBatchRun {
        selected_job_ids,
        queue_checkpoints,
        expected_plans,
        batch_plan_fingerprints,
        plan_snapshot,
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
        BatchExecutionMode, BatchLaunchRequest, SelectedBatchJob, admit_batch_launch,
        batch_project_identity, decode_persisted_batch_profile, prepare_batch_run,
        prepare_selected_batch_jobs,
    };
    use crate::core::MAX_PERSISTED_PROFILE_BYTES;
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
    fn targeted_batch_run_maps_original_queue_index_to_full_durable_queue() {
        let selected_job = BulkJob {
            label: "third mailbox".into(),
            form: Form::default(),
            state: "imported".into(),
        };
        let prepared = prepare_batch_run(
            &[SelectedBatchJob {
                queue_index: 2,
                durable_job_id: "C1".into(),
                job: selected_job,
                admission: None,
            }],
            BatchExecutionMode::Preflight,
        )
        .expect("targeted row should map into the full durable queue");

        assert_eq!(prepared.selected_job_ids, vec!["C1"]);
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

        let mut queue_form = form.clone();
        queue_form.profile.name = "New migration".into();
        let fallback = batch_project_identity(
            &[BulkJob {
                label: "mailbox".into(),
                form: queue_form,
                state: "imported".into(),
            }],
            &form.profile,
        );
        assert_eq!(fallback.name, "Customer cutover");
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

    #[test]
    fn targeted_admission_preserves_project_and_full_queue_for_every_selection_shape() {
        for selected_positions in [
            vec![0],
            vec![2],
            vec![1, 3],
            vec![0, 3],
            vec![0, 1, 2, 3],
            Vec::new(),
        ] {
            let store = crate::core::StateStore::in_memory().unwrap();
            let jobs = (0..4)
                .map(|index| {
                    let mut form = Form::default();
                    form.profile.source_host = "source.example".into();
                    form.profile.destination_host = "destination.example".into();
                    form.profile.source_user = format!("source-{index}@example.com");
                    form.profile.destination_user = format!("destination-{index}@example.com");
                    form.source_password = "source-secret".into();
                    form.destination_password = "destination-secret".into();
                    BulkJob {
                        label: format!("mailbox-{index}"),
                        form,
                        state: "queued".into(),
                    }
                })
                .collect::<Vec<_>>();
            let mailboxes = jobs
                .iter()
                .map(|job| {
                    Ok((
                        job.form.profile.source_user.clone(),
                        job.form.profile.destination_user.clone(),
                        super::durable_batch_profile_config(&job.form.profile)?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()
                .unwrap();
            let (project, queue_job_ids) = store
                .create_project_with_mailbox_configs(
                    "targeted batch",
                    "source.example",
                    "destination.example",
                    &mailboxes,
                )
                .unwrap();
            let original_project_id = project.id.clone();
            let original_job_ids = queue_job_ids.clone();
            let selected_ids = selected_positions
                .iter()
                .map(|&index| queue_job_ids[index].clone())
                .collect::<HashSet<_>>();
            let admission = admit_batch_launch(BatchLaunchRequest {
                store: &store,
                requested_project_id: Some(&project.id),
                source_jobs: &jobs,
                queue_job_ids: &queue_job_ids,
                selected_ids: &selected_ids,
                retry_scope: super::BulkRetryScope::All,
                mode: BatchExecutionMode::Preflight,
                fallback_profile: &jobs[0].form.profile,
                expected_credential_fingerprints: &[None, None, None, None],
                run_id: "targeted-selection-test",
            })
            .unwrap();

            assert_eq!(admission.project_id, original_project_id);
            assert_eq!(admission.job_ids, original_job_ids);
            let expected = if selected_positions.is_empty() {
                original_job_ids.clone()
            } else {
                selected_positions
                    .iter()
                    .map(|&index| original_job_ids[index].clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(admission.prepared.selected_job_ids, expected);
            assert_eq!(admission.active_run.project_id, original_project_id);
            assert_eq!(admission.active_run.batch_job_ids, expected);
        }
    }

    #[test]
    fn existing_project_mismatch_refuses_subset_replacement() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let error = super::prepare_batch_project(
            &store,
            Some("missing-project"),
            &[("source".into(), "destination".into(), "config".into())],
            "replacement",
            "source.example",
            "destination.example",
        )
        .expect_err("a missing requested project must not create a replacement");
        assert!(error.contains("no longer matches the admitted queue"));
        assert!(store.latest_project().unwrap().is_none());
    }

    #[test]
    fn first_batch_admission_creates_one_full_project_before_selecting_rows() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let jobs = (0..3)
            .map(|index| {
                let mut form = Form::default();
                form.profile.source_host = "source.example".into();
                form.profile.destination_host = "destination.example".into();
                form.profile.source_user = format!("source-{index}@example.com");
                form.profile.destination_user = format!("destination-{index}@example.com");
                form.source_password = "source-secret".into();
                form.destination_password = "destination-secret".into();
                BulkJob {
                    label: format!("mailbox-{index}"),
                    form,
                    state: "imported".into(),
                }
            })
            .collect::<Vec<_>>();
        let admission = admit_batch_launch(BatchLaunchRequest {
            store: &store,
            requested_project_id: None,
            source_jobs: &jobs,
            queue_job_ids: &[],
            selected_ids: &HashSet::new(),
            retry_scope: super::BulkRetryScope::All,
            mode: BatchExecutionMode::Preflight,
            fallback_profile: &jobs[0].form.profile,
            expected_credential_fingerprints: &[],
            run_id: "first-batch-admission-test",
        })
        .unwrap();

        assert_eq!(admission.job_ids.len(), jobs.len());
        assert_eq!(admission.prepared.selected_job_ids, admission.job_ids);
        assert_eq!(admission.selected_jobs.len(), jobs.len());
        assert_eq!(
            store.mailboxes(&admission.project_id).unwrap().len(),
            jobs.len()
        );
    }

    #[test]
    fn persisted_batch_profile_decoding_rejects_missing_or_corrupt_plans() {
        let missing = decode_persisted_batch_profile(None, "job-missing")
            .err()
            .expect("missing plan must be rejected");
        assert!(missing.contains("job-missing"));

        let corrupt = decode_persisted_batch_profile(Some("not = valid = toml"), "job-corrupt")
            .err()
            .expect("corrupt plan must be rejected");
        assert!(corrupt.contains("job-corrupt"));

        let profile = Form::default().profile;
        let encoded = toml::to_string(&profile).unwrap();
        assert!(decode_persisted_batch_profile(Some(&encoded), "job-valid").is_ok());

        let oversized = "x".repeat(MAX_PERSISTED_PROFILE_BYTES + 1);
        let error = decode_persisted_batch_profile(Some(&oversized), "job-large")
            .err()
            .expect("oversized plan must be rejected");
        assert!(error.contains("exceeds"));
    }
}
