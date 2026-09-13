use crate::core;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunKind {
    Single,
    Batch,
}

#[derive(Clone)]
pub(crate) struct ActiveRunContext {
    pub(crate) run_id: String,
    pub(crate) project_id: String,
    pub(crate) job_id: Option<String>,
    pub(crate) batch_job_ids: Vec<String>,
    pub(crate) batch_child_run_ids: Vec<String>,
    pub(crate) batch_child_indices: HashMap<String, usize>,
    pub(crate) batch_plan_fingerprints: Vec<String>,
    pub(crate) kind: RunKind,
    pub(crate) dry_run: bool,
    pub(crate) engine: core::Engine,
    pub(crate) plan_fingerprint: String,
    pub(crate) credential_fingerprint: String,
}

impl ActiveRunContext {
    pub(crate) fn batch_child_index(&self, job_id: &str, child_run_id: &str) -> Option<usize> {
        let index = self.batch_child_indices.get(child_run_id).copied()?;
        (self.batch_job_ids.get(index).map(String::as_str) == Some(job_id)
            && self.batch_child_run_ids.get(index).map(String::as_str) == Some(child_run_id))
        .then_some(index)
    }

    pub(crate) fn owns_batch_child(
        &self,
        parent_run_id: &str,
        child_run_id: &str,
        job_id: &str,
    ) -> bool {
        matches!(self.kind, RunKind::Batch)
            && self.run_id == parent_run_id
            && self.batch_child_index(job_id, child_run_id).is_some()
    }

    pub(crate) fn owns_process(&self, process_run_id: &str, job_id: &str) -> bool {
        if self.run_id == process_run_id {
            return matches!(self.kind, RunKind::Single) && self.job_id.as_deref() == Some(job_id);
        }
        matches!(self.kind, RunKind::Batch)
            && self.batch_child_index(job_id, process_run_id).is_some()
    }
}

/// Proof that credentials were freshly authenticated for the exact plan about
/// to be executed.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LiveAuthProof {
    pub(crate) plan_fingerprint: String,
    pub(crate) credential_fingerprint: String,
}

impl LiveAuthProof {
    pub(crate) fn matches(&self, plan_fingerprint: &str, credential_fingerprint: &str) -> bool {
        self.plan_fingerprint == plan_fingerprint
            && self.credential_fingerprint == credential_fingerprint
    }
}
