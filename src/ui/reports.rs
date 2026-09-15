//! Report-export actions exposed by the verification workspace.

use crate::atomic_artifact::write_private_atomic;
use crate::reports;
use crate::{App, export_support_bundle, persistent_state_path};

impl App {
    pub(crate) fn export_verification_report(&self) -> Result<(), String> {
        let job = self
            .job_id
            .as_deref()
            .ok_or("No mailbox evidence is available yet.")?;
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-verification.md")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let report = reports::operator::build_verification_report(&self.store, project_id, job)?;
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    pub(crate) fn export_project_report(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-report.md")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let report = reports::operator::build_project_report(&self.store, project_id)?;
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    pub(crate) fn export_project_json(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-report.json")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let report = reports::operator::build_project_json(&self.store, project_id)?;
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    /// Export the customer-safe proof artifact. Unlike the operator JSON
    /// report, this intentionally omits project IDs, endpoints, plan
    /// snapshots, credential references, executable paths, and diagnostic
    /// details that may reveal internal topology.
    pub(crate) fn export_customer_proof(&self) -> Result<(), String> {
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-customer-proof.json")
            .save_file()
            .ok_or("Customer proof export cancelled.")?;
        self.export_customer_proof_to(&path)
    }

    pub(crate) fn export_customer_proof_to(&self, path: &std::path::Path) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        reports::customer::export_from_store(&self.store, project_id, path, &self.branding)
    }

    pub(crate) fn export_support_bundle_dialog(&self) -> Result<(), String> {
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-support-bundle.json")
            .save_file()
            .ok_or("Support-bundle export cancelled.")?;
        let state_path = persistent_state_path()?;
        export_support_bundle(&state_path, &path)
    }

    pub(crate) fn export_project_health(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-health.json")
            .save_file()
            .ok_or("Health export cancelled.")?;
        reports::operator::export_health(&self.store, project_id, &path)
    }
}
