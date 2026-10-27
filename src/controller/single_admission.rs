//! Durable admission for a single mailbox run.

use super::run::{ActiveRunContext, RunKind};
use crate::core;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SingleStartBlock {
    StaleDurableView,
    ProfileUnavailable,
    ReadOnlyProject,
    ProcessReviewRequired,
}

impl SingleStartBlock {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::StaleDurableView => {
                "Execution is blocked while the durable state view is stale. Resolve the SQLite refresh error and refresh before starting a migration."
            }
            Self::ProfileUnavailable => {
                "Execution is blocked because the saved migration profile is unavailable; repair it before starting a migration."
            }
            Self::ReadOnlyProject => {
                "This project is being viewed read-only. Start a new migration to execute a plan."
            }
            Self::ProcessReviewRequired => {
                "Execution is blocked until you confirm that no unverified migration process remains on this host."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SingleStartDecision {
    Block(SingleStartBlock),
    ConfirmLive,
    Proceed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SingleStartContext {
    pub(crate) durable_view_stale: bool,
    pub(crate) profile_available: bool,
    pub(crate) read_only_project: bool,
    pub(crate) process_review_required: bool,
    pub(crate) live_requires_confirmation: bool,
}

pub(crate) fn single_start_decision(context: SingleStartContext) -> SingleStartDecision {
    if context.durable_view_stale {
        return SingleStartDecision::Block(SingleStartBlock::StaleDurableView);
    }
    if !context.profile_available {
        return SingleStartDecision::Block(SingleStartBlock::ProfileUnavailable);
    }
    if context.read_only_project {
        return SingleStartDecision::Block(SingleStartBlock::ReadOnlyProject);
    }
    if context.process_review_required {
        return SingleStartDecision::Block(SingleStartBlock::ProcessReviewRequired);
    }
    if context.live_requires_confirmation {
        return SingleStartDecision::ConfirmLive;
    }
    SingleStartDecision::Proceed
}

pub(crate) struct SingleRunAdmission {
    pub(crate) project_id: String,
    pub(crate) job_id: String,
    pub(crate) run_id: String,
    pub(crate) engine: core::Engine,
    pub(crate) dry_run: bool,
    pub(crate) plan_fingerprint: String,
    pub(crate) credential_fingerprint: String,
    pub(crate) plan_snapshot: String,
    pub(crate) source_provider: String,
    pub(crate) destination_provider: String,
    pub(crate) organization_policy: Option<crate::organization_policy::OrganizationPolicy>,
}

pub(crate) fn policy_snapshot_for_admission(
    dry_run: bool,
) -> Result<Option<crate::organization_policy::OrganizationPolicy>, String> {
    policy_snapshot_for_admission_from(
        dry_run,
        crate::organization_policy::OrganizationPolicy::load,
    )
}

fn policy_snapshot_for_admission_from(
    dry_run: bool,
    load: impl FnOnce() -> Result<crate::organization_policy::OrganizationPolicy, String>,
) -> Result<Option<crate::organization_policy::OrganizationPolicy>, String> {
    match load() {
        Ok(policy) => Ok(Some(policy)),
        Err(_) if dry_run => Ok(None),
        Err(error) => Err(error),
    }
}

/// Commit a single run snapshot and construct the matching ownership context.
/// The UI may still prepare engine resources, but it does not decide how a
/// durable run and its process-event identity fit together.
pub(crate) fn admit_single_run(
    store: &core::StateStore,
    admission: SingleRunAdmission,
) -> Result<ActiveRunContext, String> {
    if !admission.dry_run {
        let expected_policy = admission.organization_policy.as_ref().ok_or_else(|| {
            "Organization policy was not captured; live migration was not started.".to_owned()
        })?;
        let current_policy =
            crate::organization_policy::OrganizationPolicy::load().map_err(|error| {
                format!("Could not recheck organization policy; no migration was started: {error}")
            })?;
        if &current_policy != expected_policy {
            return Err(
                "Organization policy changed during admission; review the plan and start again. No migration was started.".into(),
            );
        }
    }
    store
        .begin_run_with_snapshot(
            &admission.project_id,
            &admission.job_id,
            &admission.run_id,
            admission.engine.label(),
            &admission.plan_snapshot,
        )
        .map_err(|error| format!("Could not record durable run; nothing was started: {error}"))?;
    Ok(ActiveRunContext {
        run_id: admission.run_id,
        project_id: admission.project_id,
        job_id: Some(admission.job_id),
        batch_job_ids: Vec::new(),
        batch_child_run_ids: Vec::new(),
        batch_child_indices: std::collections::HashMap::new(),
        batch_plan_fingerprints: Vec::new(),
        kind: RunKind::Single,
        dry_run: admission.dry_run,
        plan_fingerprint: admission.plan_fingerprint,
        credential_fingerprint: admission.credential_fingerprint,
        source_provider: admission.source_provider,
        destination_provider: admission.destination_provider,
        verification_state_path: None,
        required_assurance: admission
            .organization_policy
            .as_ref()
            .and_then(crate::organization_policy::OrganizationPolicy::required_assurance),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        SingleStartBlock, SingleStartContext, SingleStartDecision,
        policy_snapshot_for_admission_from, single_start_decision,
    };

    fn context() -> SingleStartContext {
        SingleStartContext {
            durable_view_stale: false,
            profile_available: true,
            read_only_project: false,
            process_review_required: false,
            live_requires_confirmation: false,
        }
    }

    #[test]
    fn single_start_gate_requires_live_confirmation() {
        assert_eq!(
            single_start_decision(context()),
            SingleStartDecision::Proceed
        );
        let mut live = context();
        live.live_requires_confirmation = true;
        assert_eq!(
            single_start_decision(live),
            SingleStartDecision::ConfirmLive
        );
    }

    #[test]
    fn single_start_gate_prioritizes_durable_safety_blocks() {
        let mut stale = context();
        stale.durable_view_stale = true;
        stale.live_requires_confirmation = true;
        assert_eq!(
            single_start_decision(stale),
            SingleStartDecision::Block(SingleStartBlock::StaleDurableView)
        );
    }

    #[test]
    fn unreadable_policy_blocks_live_admission_but_allows_dry_preflight() {
        let load = || Err("policy file is unreadable".to_owned());

        assert_eq!(
            policy_snapshot_for_admission_from(false, load),
            Err("policy file is unreadable".to_owned())
        );
        assert_eq!(policy_snapshot_for_admission_from(true, load), Ok(None));
    }
}
