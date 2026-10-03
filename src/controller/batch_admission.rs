//! Batch admission and queue policy shared by GUI and headless callers.

use super::batch::BatchExecutionMode;
use super::batch::{BatchActionPlanBuilder, BatchActionRow, BulkRetryScope, SelectionScope};
use super::queue::SessionSecrets;
use super::run::{ActiveRunContext, RunKind};
use crate::core::{MAX_PERSISTED_PROFILE_BYTES, MAX_TOTAL_PERSISTED_PROFILE_BYTES};
use crate::{Profile, bulk_import::BulkJob, core, effective_destination_tls, endpoint};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub(crate) struct BatchProjectIdentity {
    pub(crate) name: String,
    pub(crate) source_endpoint: String,
    pub(crate) destination_endpoint: String,
}

pub(crate) struct BatchLaunchAdmission {
    pub(crate) prepared: PreparedBatchRun,
    pub(crate) active_run: ActiveRunContext,
}

pub(crate) struct BatchLaunchRequest<'a> {
    pub(crate) store: &'a core::StateStore,
    /// The durable batch whose rows are the queue.
    pub(crate) project_id: &'a str,
    pub(crate) selection_scope: &'a SelectionScope,
    pub(crate) retry_scope: BulkRetryScope,
    pub(crate) mode: BatchExecutionMode,
    /// Concurrency, throughput, and expert options come from the operator's
    /// current profile; expert options are never persisted per row.
    pub(crate) fallback_profile: &'a Profile,
    /// Credential bindings recorded by this session's successful dry runs.
    pub(crate) expected_credential_fingerprints: &'a HashMap<String, String>,
    pub(crate) session_secrets: &'a SessionSecrets,
    /// Hash of the live confirmation plan, when confirmation was required.
    /// Admission recomputes it from the same durable rows before execution.
    pub(crate) expected_action_plan_hash: Option<&'a str>,
    /// Explicit acknowledgement shown in the live confirmation when
    /// case-folded destination names collide on an endpoint with unknown
    /// account-name case semantics.
    pub(crate) acknowledge_ambiguous_destination_case: bool,
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
/// GUI and headless batch launches. The queue is read from the ledger: a
/// scan selects rows, and each selected row's plan is decoded, validated,
/// and digested one at a time, so admission never holds the queue's plans
/// (or credentials) in memory.
pub(crate) fn admit_batch_launch(
    request: BatchLaunchRequest<'_>,
) -> Result<BatchLaunchAdmission, String> {
    let BatchLaunchRequest {
        store,
        project_id,
        selection_scope,
        retry_scope,
        mode,
        fallback_profile,
        expected_credential_fingerprints,
        session_secrets,
        expected_action_plan_hash,
        acknowledge_ambiguous_destination_case,
        run_id,
    } = request;
    let concurrency =
        crate::migration_plan::effective_batch_concurrency(fallback_profile.batch_concurrency);
    let organization_policy = crate::organization_policy::OrganizationPolicy::load()
        .map_err(|error| format!("Batch admission blocked by organization policy: {error}"))?;
    organization_policy
        .check(fallback_profile)
        .map_err(|error| format!("Batch admission blocked by organization policy: {error}"))?;
    let mut plan = build_plan_builder(retry_scope, fallback_profile, mode);
    let mut selected_ids = Vec::new();
    store
        .queue_durable_scan(project_id, |row| {
            let selected = selection_scope.contains(Some(&row.id));
            plan.add(BatchActionRow {
                id: &row.id,
                selected,
                visible: true,
                durable_state: Some(&row.durable_state),
                destructive: row.destructive,
            });
            if super::batch::batch_row_admitted(
                selection_scope,
                retry_scope,
                &row.id,
                &row.durable_state,
            ) {
                selected_ids.push(row.id);
            }
        })
        .map_err(|error| {
            format!("Could not read the durable batch queue; batch was not started: {error}")
        })?;
    if let Some(expected_hash) = expected_action_plan_hash
        && plan.finish().identity_hash != expected_hash
    {
        return Err(
            "The confirmed batch action plan is stale; durable mailbox state or destructive settings changed. Review the selection and confirm again.".into(),
        );
    }
    if selected_ids.is_empty() {
        return Err(format!(
            "No mailboxes match the selected live retry scope: {}.",
            retry_scope.label()
        ));
    }
    let admissions = store
        .batch_admission_states(project_id, &selected_ids)
        .map_err(|error| {
            format!("Could not read durable batch admission state; batch was not started: {error}")
        })?
        .into_iter()
        .map(|admission| (admission.job_id.clone(), admission))
        .collect::<HashMap<_, _>>();
    let provider_rate_ceilings = organization_policy
        .provider_rate_ceilings()
        .map_err(|error| format!("Batch admission blocked by organization policy: {error}"))?;
    let mut preparation =
        BatchRunPreparation::new(mode, selected_ids.len(), provider_rate_ceilings);
    let mut destinations = HashSet::new();
    let mut case_collisions = CaseCollisionDetector::default();
    let mut ambiguous_case_collision = false;
    for chunk in selected_ids.chunks(500) {
        let plans = store
            .queue_plans(project_id, chunk)
            .map_err(|error| format!("Could not read durable batch plans: {error}"))?;
        if plans.len() != chunk.len() {
            return Err(
                "Selected batch rows could not be resolved in the admitted project; refusing execution."
                    .into(),
            );
        }
        for row in plans {
            let label = row.label.clone();
            let mut form = crate::controller::queue::job_from_plan_with_batch_policy(
                &row,
                fallback_profile,
                session_secrets.get(&row.id),
                mode.is_preflight(),
            )?
            .form();
            let admission = admissions.get(&row.id);
            if mode.is_live() {
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
                        crate::plan_identity::fingerprint_digest(&form.plan_fingerprint()).as_str(),
                    )
                {
                    return Err(format!(
                        "Mailbox {label} is not ready for live execution. Re-run dry validation after reviewing its exact plan."
                    ));
                }
            }
            // Live admission must not mint queue-wide OAuth access tokens: a
            // selected mailbox may wait behind many workers, and its worker
            // refreshes immediately before authentication and launch. These
            // credentials are loaded only to validate and are then dropped;
            // each worker loads its own.
            let credential_load = if mode.is_live() {
                form.load_static_configured_keyring_credentials()
            } else {
                form.load_configured_keyring_credentials()
            };
            if let Err(error) = credential_load {
                return Err(format!(
                    "Could not load credentials for mailbox {label} before durable admission: {error}"
                ));
            }
            if mode.is_live()
                && expected_credential_fingerprints
                    .get(&row.id)
                    .map(String::as_str)
                    != Some(form.credential_binding_fingerprint().as_str())
            {
                return Err(format!(
                    "Mailbox {label} credentials changed or were not retained from dry validation. Run a new dry validation before live execution."
                ));
            }
            form.validate()
                .map_err(|error| format!("Mailbox {label} is not ready for validation: {error}"))?;
            organization_policy.check(&form.profile).map_err(|error| {
                format!("Mailbox {label} is blocked by organization policy: {error}")
            })?;
            if mode.is_live() && form.requires_insecure_transport_ack() {
                return Err("Live batch blocked: explicitly acknowledge that plain IMAP exposes credentials and mail in transit for every affected row.".into());
            }
            if mode.is_live() && crate::runner::automap_blocks_live_certification(&form) {
                return Err("Live batch blocked: one or more selected imapsync plans use automapping, which cannot currently be independently verified. Disable automap and rerun preflight for those mailboxes.".into());
            }
            crate::runner::validate_body_hash_limits(&form)?;
            if mode.is_live()
                && !destinations.insert(canonical_destination_identity(&form.profile)?)
            {
                return Err(format!(
                    "Mailbox {label} targets a destination mailbox already used by another batch row; concurrent writes to one mailbox are blocked."
                ));
            }
            ambiguous_case_collision |= case_collisions.add(&form.profile)?;
            let checkpoint = if mode.is_live() && form.engine() == core::Engine::Dovecot {
                admission.and_then(|admission| admission.checkpoint.clone())
            } else {
                None
            };
            preparation.add(row.id, &form, checkpoint)?;
        }
    }
    validate_destination_case_acknowledgement(
        mode,
        ambiguous_case_collision,
        acknowledge_ambiguous_destination_case,
        concurrency,
    )?;
    validate_batch_throttle_with_policy(fallback_profile, concurrency, &organization_policy)?;
    let prepared = preparation.finish()?;
    let active_run = admit_batch_run(store, project_id, run_id, mode, prepared.clone())?;
    Ok(BatchLaunchAdmission {
        prepared,
        active_run,
    })
}

fn build_plan_builder(
    retry_scope: BulkRetryScope,
    fallback_profile: &Profile,
    mode: BatchExecutionMode,
) -> BatchActionPlanBuilder {
    BatchActionPlanBuilder::new(
        retry_scope,
        crate::migration_plan::effective_batch_concurrency(fallback_profile.batch_concurrency),
        mode,
    )
}

/// Derive durable batch-project metadata from the admitted queue. Keeping
/// this together with admission ensures GUI and headless callers use the same
/// naming and endpoint contract.
pub(crate) fn batch_project_identity(
    jobs: &[BulkJob],
    fallback_profile: &Profile,
) -> BatchProjectIdentity {
    let queue_profile = jobs.first().map(BulkJob::profile);
    let queue_profile_ref = queue_profile.as_ref().unwrap_or(fallback_profile);
    let configured_fallback = fallback_profile.name.trim();
    let profile = if !configured_fallback.is_empty()
        && !matches!(
            configured_fallback,
            "New migration" | "Batch migration" | "Batch validation"
        )
        && (queue_profile_ref.name.trim().is_empty()
            || matches!(
                queue_profile_ref.name.trim(),
                "New migration" | "Batch migration" | "Batch validation"
            )) {
        fallback_profile
    } else {
        queue_profile_ref
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
#[cfg(test)]
pub(crate) fn durable_batch_profile_config(profile: &Profile) -> Result<String, String> {
    let mut safe_profile = profile.clone();
    safe_profile.extra_options.clear();
    toml::to_string(&safe_profile)
        .map_err(|error| format!("Could not serialize batch plan: {error}"))
}

/// Secret-free export of selected queue rows. Credentials, credential
/// references, and engine options are not part of a queue row.
pub(crate) fn selection_value(rows: &[core::QueueRow]) -> serde_json::Value {
    let rows = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "label": row.label,
                "source_host": row.source_host,
                "source_user": row.source_user,
                "destination_host": row.destination_host,
                "destination_user": row.destination_user,
                "state": crate::ui::display_state_key(&row.state),
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
#[allow(dead_code)]
pub(crate) fn validate_batch_throttle(profile: &Profile, concurrency: usize) -> Result<(), String> {
    let organization_policy = crate::organization_policy::OrganizationPolicy::load()?;
    validate_batch_throttle_with_policy(profile, concurrency, &organization_policy)
}

fn validate_batch_throttle_with_policy(
    profile: &Profile,
    concurrency: usize,
    organization_policy: &crate::organization_policy::OrganizationPolicy,
) -> Result<(), String> {
    organization_policy.check_form(&crate::Form {
        profile: profile.clone(),
        source_password: crate::credentials::SecretString::default(),
        destination_password: crate::credentials::SecretString::default(),
        dry_run: false,
    })?;
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
        let profile = job.profile();
        let key = canonical_destination_identity(&profile)?;
        if !destinations.insert(key) {
            return Ok(Some(format!(
                "Mailbox {} targets a destination mailbox already used by another batch row; concurrent writes to one mailbox are blocked.",
                index + 1
            )));
        }
    }
    Ok(None)
}

/// An unknown provider may treat mailbox names as case-insensitive, so a pair
/// that differs only by case must be surfaced even though its exact identity
/// remains distinct. Known Gmail and Exchange Online endpoints already use a
/// case-insensitive canonical identity and are handled as exact duplicates.
#[cfg(test)]
pub(crate) fn has_ambiguous_destination_casefold_collision<'a>(
    jobs: impl Iterator<Item = &'a BulkJob>,
) -> Result<bool, String> {
    let mut detector = CaseCollisionDetector::default();
    for job in jobs {
        if detector.add(&job.profile())? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Streaming form of the case-collision check, for selections read from
/// the durable queue one plan at a time.
#[derive(Default)]
pub(crate) struct CaseCollisionDetector {
    by_folded: std::collections::HashMap<String, String>,
}

impl CaseCollisionDetector {
    /// Record one destination; returns whether it collides by case with an
    /// earlier one.
    pub(crate) fn add(&mut self, profile: &Profile) -> Result<bool, String> {
        let exact = canonical_destination_identity(profile)?;
        let folded = endpoint::casefolded_destination_identity(
            &profile.destination_user,
            &profile.destination_host,
            effective_destination_tls(&profile.destination_tls),
            &profile.destination_port,
        )
        .map_err(|error| format!("Invalid destination endpoint: {error}"))?;
        if self
            .by_folded
            .get(&folded)
            .is_some_and(|previous| previous != &exact)
        {
            return Ok(true);
        }
        self.by_folded.insert(folded, exact);
        Ok(false)
    }
}

fn validate_destination_case_acknowledgement(
    mode: BatchExecutionMode,
    has_collision: bool,
    acknowledged: bool,
    concurrency: usize,
) -> Result<(), String> {
    if mode.is_live() && has_collision {
        if !acknowledged {
            return Err("Live batch blocked: destination mailbox names differ only by case on an endpoint with unknown case semantics. Review the accounts and explicitly acknowledge the possible identity collision in the live confirmation.".into());
        }
        if concurrency != 1 {
            return Err("Live batch blocked: case-ambiguous destination accounts must run with concurrency set to 1 so possible aliases are never written simultaneously.".into());
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct PreparedBatchRun {
    /// Distinct imapsync executables the selected plans use, resolved once
    /// before any mailbox process starts.
    pub(crate) imapsync_executables: Vec<String>,
    pub(crate) selected_job_ids: Vec<String>,
    pub(crate) queue_checkpoints: Vec<Option<String>>,
    pub(crate) expected_plans: Vec<String>,
    pub(crate) batch_plan_fingerprints: Vec<String>,
    pub(crate) plan_snapshot: String,
    pub(crate) child_plans: Vec<core::BatchChildPlan>,
    pub(crate) provider_rate_ceilings: crate::organization_policy::ProviderRateCeilings,
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

/// Accumulates the exact durable inputs for an admitted batch, one
/// validated row at a time. This is a deterministic controller operation and
/// deliberately has no egui or process-launch responsibilities.
pub(crate) struct BatchRunPreparation {
    mode: BatchExecutionMode,
    provider_rate_ceilings: crate::organization_policy::ProviderRateCeilings,
    selected_job_ids: Vec<String>,
    queue_checkpoints: Vec<Option<String>>,
    batch_plan_fingerprints: Vec<String>,
    child_plans: Vec<core::BatchChildPlan>,
    total_snapshot_bytes: usize,
    batch_digest: Sha256,
    imapsync_executables: std::collections::BTreeSet<String>,
}

impl BatchRunPreparation {
    pub(crate) fn new(
        mode: BatchExecutionMode,
        capacity: usize,
        provider_rate_ceilings: crate::organization_policy::ProviderRateCeilings,
    ) -> Self {
        Self {
            mode,
            provider_rate_ceilings,
            selected_job_ids: Vec::with_capacity(capacity),
            queue_checkpoints: Vec::with_capacity(capacity),
            batch_plan_fingerprints: Vec::with_capacity(capacity),
            child_plans: Vec::with_capacity(capacity),
            total_snapshot_bytes: 0,
            batch_digest: Sha256::new(),
            imapsync_executables: std::collections::BTreeSet::new(),
        }
    }

    /// Add one validated row with the checkpoint its live run resumes from.
    pub(crate) fn add(
        &mut self,
        job_id: String,
        form: &crate::Form,
        checkpoint: Option<String>,
    ) -> Result<(), String> {
        if self.mode.is_live()
            && form.engine() == core::Engine::Dovecot
            && checkpoint
                .as_deref()
                .is_some_and(|value| core::dovecot_checkpoint_context(value).is_none())
        {
            return Err(format!(
                "Mailbox {job_id} has a legacy Dovecot checkpoint without UIDVALIDITY context; run a fresh full pass before resuming."
            ));
        }
        let plan_snapshot = form.plan_snapshot_with_checkpoint(checkpoint.as_deref())?;
        self.total_snapshot_bytes = self
            .total_snapshot_bytes
            .checked_add(plan_snapshot.len())
            .ok_or_else(|| "Batch plan snapshots exceed the aggregate size budget".to_owned())?;
        if self.total_snapshot_bytes > MAX_TOTAL_PERSISTED_PROFILE_BYTES {
            return Err(format!(
                "Batch plan snapshots exceed the aggregate {MAX_TOTAL_PERSISTED_PROFILE_BYTES}-byte budget"
            ));
        }
        self.batch_digest
            .update((plan_snapshot.len() as u64).to_le_bytes());
        self.batch_digest.update(plan_snapshot.as_bytes());
        self.batch_plan_fingerprints
            .push(crate::plan_identity::fingerprint_digest(
                &form.plan_fingerprint(),
            ));
        self.child_plans.push(core::BatchChildPlan {
            engine: form.engine().label().to_owned(),
            plan_snapshot,
            engine_version: None,
        });
        if form.engine() == core::Engine::ImapSync {
            self.imapsync_executables
                .insert(form.profile.imapsync_path.clone());
        }
        self.selected_job_ids.push(job_id);
        self.queue_checkpoints.push(checkpoint);
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<PreparedBatchRun, String> {
        if self.selected_job_ids.is_empty() {
            return Err(
                "Batch admission resolved zero selected durable jobs; refusing to start.".into(),
            );
        }
        let batch_plan_digest = self
            .batch_digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let plan_snapshot = format!(
            "batch_plan_count={}\nbatch_plan_sha256={batch_plan_digest}\n",
            self.child_plans.len()
        );
        Ok(PreparedBatchRun {
            imapsync_executables: self.imapsync_executables.into_iter().collect(),
            expected_plans: if self.mode.is_live() {
                self.batch_plan_fingerprints.clone()
            } else {
                Vec::new()
            },
            selected_job_ids: self.selected_job_ids,
            queue_checkpoints: self.queue_checkpoints,
            batch_plan_fingerprints: self.batch_plan_fingerprints,
            plan_snapshot,
            child_plans: self.child_plans,
            provider_rate_ceilings: self.provider_rate_ceilings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BatchExecutionMode, BatchLaunchRequest, admit_batch_launch, batch_project_identity,
        decode_persisted_batch_profile, duplicate_destination,
        has_ambiguous_destination_casefold_collision, validate_destination_case_acknowledgement,
    };
    use crate::controller::queue::{SessionSecrets, persist_imported_queue};
    use crate::controller::{BatchActionRow, BulkRetryScope, SelectionScope};
    use crate::core::MAX_PERSISTED_PROFILE_BYTES;
    use crate::{bulk_import::BulkJob, migration_plan::Form};
    use std::collections::{HashMap, HashSet};

    /// A durable queue of `count` rows imported with session passwords.
    fn queue(
        store: &crate::core::StateStore,
        count: usize,
    ) -> (String, Vec<String>, SessionSecrets) {
        let jobs = (0..count)
            .map(|index| {
                let mut form = Form::default();
                form.profile.source_host = "source.example".into();
                form.profile.destination_host = "destination.example".into();
                form.profile.source_user = format!("source-{index}@example.com");
                form.profile.destination_user = format!("destination-{index}@example.com");
                form.source_password = "source-secret".into();
                form.destination_password = "destination-secret".into();
                BulkJob::from_form(format!("mailbox-{index}"), form, "imported".into())
            })
            .collect::<Vec<_>>();
        let imported = persist_imported_queue(store, jobs, &Form::default().profile).unwrap();
        let ids = store.mailbox_ids(&imported.project_id).unwrap();
        (imported.project_id, ids, imported.session_secrets)
    }

    fn admit(
        store: &crate::core::StateStore,
        project_id: &str,
        selection: &SelectionScope,
        mode: BatchExecutionMode,
        secrets: &SessionSecrets,
        expected_hash: Option<&str>,
        run_id: &str,
    ) -> Result<super::BatchLaunchAdmission, String> {
        admit_batch_launch(BatchLaunchRequest {
            store,
            project_id,
            selection_scope: selection,
            retry_scope: BulkRetryScope::All,
            mode,
            fallback_profile: &Form::default().profile,
            expected_credential_fingerprints: &HashMap::new(),
            session_secrets: secrets,
            expected_action_plan_hash: expected_hash,
            acknowledge_ambiguous_destination_case: false,
            run_id,
        })
    }

    #[test]
    fn admission_selects_exact_rows_in_queue_order_for_every_selection_shape() {
        for selected_positions in [
            vec![0],
            vec![2],
            vec![3, 1],
            vec![0, 3],
            vec![0, 1, 2, 3],
            Vec::new(),
        ] {
            let store = crate::core::StateStore::in_memory().unwrap();
            let (project_id, ids, secrets) = queue(&store, 4);
            let selection = if selected_positions.is_empty() {
                SelectionScope::all_matching()
            } else {
                SelectionScope::Explicit(
                    selected_positions
                        .iter()
                        .map(|&index| ids[index].clone())
                        .collect(),
                )
            };
            let admission = admit(
                &store,
                &project_id,
                &selection,
                BatchExecutionMode::Preflight,
                &secrets,
                None,
                "selection-shape",
            )
            .unwrap();
            let mut positions = if selected_positions.is_empty() {
                (0..4).collect::<Vec<_>>()
            } else {
                selected_positions.clone()
            };
            positions.sort_unstable();
            let expected = positions
                .iter()
                .map(|&index| ids[index].clone())
                .collect::<Vec<_>>();
            assert_eq!(admission.prepared.selected_job_ids, expected);
            assert_eq!(admission.active_run.project_id, project_id);
            assert_eq!(admission.active_run.batch_job_ids, expected);
            assert_eq!(admission.prepared.child_plans.len(), expected.len());
            // Every row stays in the one durable project.
            assert_eq!(store.queue_len(&project_id).unwrap(), 4);
        }
    }

    #[test]
    fn session_passwords_reach_admission_without_entering_the_ledger() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let (project_id, ids, secrets) = queue(&store, 2);
        assert_eq!(secrets.len(), 2);
        let plans = store.queue_plans(&project_id, &ids).unwrap();
        assert!(
            plans
                .iter()
                .all(|plan| !plan.config.as_deref().unwrap().contains("secret"))
        );
        // Without the session passwords the rows have no credentials at all.
        let error = admit(
            &store,
            &project_id,
            &SelectionScope::all_matching(),
            BatchExecutionMode::Preflight,
            &SessionSecrets::new(),
            None,
            "no-secrets",
        )
        .err()
        .unwrap();
        assert!(error.contains("not ready for validation"), "{error}");
        admit(
            &store,
            &project_id,
            &SelectionScope::all_matching(),
            BatchExecutionMode::Preflight,
            &secrets,
            None,
            "with-secrets",
        )
        .unwrap();
    }

    #[test]
    fn live_admission_fails_closed_without_a_matching_preflight() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let (project_id, _, secrets) = queue(&store, 1);
        let error = admit(
            &store,
            &project_id,
            &SelectionScope::all_matching(),
            BatchExecutionMode::Live,
            &secrets,
            None,
            "live-without-preflight",
        )
        .err()
        .unwrap();
        assert!(error.contains("not ready for live execution"), "{error}");
    }

    #[test]
    fn empty_selection_is_rejected_before_any_run_is_recorded() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let (project_id, _, secrets) = queue(&store, 2);
        let error = admit(
            &store,
            &project_id,
            &SelectionScope::Explicit(HashSet::new()),
            BatchExecutionMode::Preflight,
            &secrets,
            None,
            "empty",
        )
        .err()
        .unwrap();
        assert!(error.contains("No mailboxes match"), "{error}");
        assert!(store.recent_run_list(&project_id, 10).unwrap().is_empty());
    }

    /// The confirmation's plan identity is recomputed by admission from the
    /// same durable rows. Rows hidden by the operator's filter are still
    /// part of the action and must not make the confirmation look stale.
    #[test]
    fn confirmed_plan_identity_survives_hidden_rows_and_rejects_drift() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let (project_id, ids, secrets) = queue(&store, 3);
        let selection = SelectionScope::Explicit(ids[..2].iter().cloned().collect());
        let mut builder = super::BatchActionPlanBuilder::new(
            BulkRetryScope::All,
            Form::default().profile.batch_concurrency,
            BatchExecutionMode::Preflight,
        );
        store
            .queue_durable_scan(&project_id, |row| {
                builder.add(BatchActionRow {
                    id: &row.id,
                    selected: selection.contains(Some(&row.id)),
                    // The second row is hidden by a filter in the GUI.
                    visible: row.id != ids[1],
                    durable_state: Some(&row.durable_state),
                    destructive: row.destructive,
                });
            })
            .unwrap();
        let confirmed = builder.finish();
        assert_eq!(confirmed.hidden_selection_count, 1);
        admit(
            &store,
            &project_id,
            &selection,
            BatchExecutionMode::Preflight,
            &secrets,
            Some(&confirmed.identity_hash),
            "confirmed",
        )
        .unwrap();
        let error = admit(
            &store,
            &project_id,
            &SelectionScope::all_matching(),
            BatchExecutionMode::Preflight,
            &secrets,
            Some(&confirmed.identity_hash),
            "drifted",
        )
        .err()
        .unwrap();
        assert!(error.contains("stale"), "{error}");
    }

    #[test]
    fn batch_project_identity_uses_queue_profile_and_safe_fallback() {
        let mut form = Form::default();
        form.profile.source_host = " imap.source.example ".into();
        form.profile.destination_host = "imap.destination.example".into();
        form.profile.name = "Customer cutover".into();
        let jobs = [BulkJob::from_form(
            "mailbox".into(),
            form.clone(),
            "imported".into(),
        )];
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
            &[BulkJob::from_form(
                "mailbox".into(),
                queue_form,
                "imported".into(),
            )],
            &form.profile,
        );
        assert_eq!(fallback.name, "Customer cutover");
    }

    #[test]
    fn casefolded_destination_collisions_are_flagged_only_when_provider_semantics_are_unknown() {
        let mut first = Form::default();
        first.profile.destination_host = "imap.customer.example".into();
        first.profile.destination_user = "User@example.test".into();
        let mut second = first.clone();
        second.profile.destination_user = "user@example.test".into();
        let generic = [
            BulkJob::from_form("first".into(), first.clone(), "Ready".into()),
            BulkJob::from_form("second".into(), second.clone(), "Ready".into()),
        ];
        assert!(has_ambiguous_destination_casefold_collision(generic.iter()).unwrap());

        first.profile.destination_host = "imap.gmail.com".into();
        second.profile.destination_host = "imap.gmail.com".into();
        let known_case_insensitive = vec![
            BulkJob::from_form("first".into(), first, "Ready".into()),
            BulkJob::from_form("second".into(), second, "Ready".into()),
        ];
        assert!(
            !has_ambiguous_destination_casefold_collision(known_case_insensitive.iter()).unwrap()
        );
        assert!(
            duplicate_destination(&known_case_insensitive)
                .unwrap()
                .is_some()
        );
        assert!(
            validate_destination_case_acknowledgement(BatchExecutionMode::Live, true, false, 1,)
                .unwrap_err()
                .contains("explicitly acknowledge")
        );
        validate_destination_case_acknowledgement(BatchExecutionMode::Live, true, true, 1).unwrap();
        assert!(
            validate_destination_case_acknowledgement(BatchExecutionMode::Live, true, true, 4,)
                .is_err()
        );
        validate_destination_case_acknowledgement(BatchExecutionMode::Preflight, true, false, 4)
            .unwrap();
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
