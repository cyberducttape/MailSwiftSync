//! Durable verification-review actions requested by operator-facing surfaces.

use crate::App;

impl App {
    /// Record an operator's acceptance of a verification difference through
    /// the controller boundary. UI code supplies the reviewed values but does
    /// not own the durable state transition.
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
}
