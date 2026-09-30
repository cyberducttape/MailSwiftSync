//! Durable domain actions requested by operator-facing surfaces.

use crate::App;

impl App {
    /// Create a durable project and its first mailbox from the reviewed plan.
    pub(crate) fn persist_new_project(
        &self,
        name: &str,
        source_host: &str,
        destination_host: &str,
        source_mailbox: &str,
        destination_mailbox: &str,
    ) -> rusqlite::Result<(crate::core::Project, String)> {
        self.store.create_project_with_mailbox(
            name,
            source_host,
            destination_host,
            source_mailbox,
            destination_mailbox,
        )
    }

    /// Record an operator's acceptance of a verification difference.
    pub(crate) fn accept_verification_difference(
        &self,
        project_id: &str,
        job_id: &str,
        operator: &str,
        reason: &str,
    ) -> rusqlite::Result<()> {
        self.store
            .accept_verification_difference(project_id, job_id, operator, reason)
    }

    /// Reopen a completed project only with the operator's durable reason.
    pub(crate) fn reopen_completed_project(
        &self,
        project_id: &str,
        reason: &str,
    ) -> rusqlite::Result<()> {
        self.store.reopen_project(project_id, reason)
    }

    /// Clear stale process identities only after the UI has collected the
    /// explicit operator acknowledgement required by the recovery gate.
    pub(crate) fn acknowledge_process_review(&self) -> rusqlite::Result<()> {
        self.store.clear_active_processes_after_review()
    }
}
