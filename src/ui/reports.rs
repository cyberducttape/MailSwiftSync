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

    pub(crate) fn export_project_report(&mut self) {
        let Some((project_id, path)) = self.choose_report_export_path(
            "Project report",
            "mailswiftsync-project-report.md",
            "Report export cancelled.",
        ) else {
            return;
        };
        self.start_report_export("Project report", move |store| {
            let report = reports::operator::build_project_report(store, &project_id)?;
            write_private_atomic(&path, &report).map_err(|e| e.to_string())
        });
    }

    /// Resolve the active project and ask for an output path, reporting a
    /// missing project or a cancelled dialog as that artifact's result.
    fn choose_report_export_path(
        &mut self,
        artifact: &'static str,
        file_name: &str,
        cancelled: &str,
    ) -> Option<(String, std::path::PathBuf)> {
        let Some(project_id) = self.active_project_id().map(str::to_owned) else {
            self.report_export_result(
                artifact,
                Err("No durable migration project is available yet.".into()),
            );
            return None;
        };
        let Some(path) = rfd::FileDialog::new().set_file_name(file_name).save_file() else {
            self.report_export_result(artifact, Err(cancelled.into()));
            return None;
        };
        Some((project_id, path))
    }

    /// Run a complete-project export off the UI thread. A fleet-scale
    /// export streams every mailbox and run and can take tens of seconds, so
    /// it uses its own read-only ledger connection and reports through
    /// `report_export_result` when done. Without a persistent ledger (an
    /// in-memory session) it runs synchronously on the session store.
    pub(crate) fn start_report_export(
        &mut self,
        artifact: &'static str,
        export: impl FnOnce(&crate::core::StateStore) -> Result<(), String> + Send + 'static,
    ) {
        if self.report_export_receiver.is_some() {
            self.set_status(
                "Another report export is still running; wait for it to finish.",
                crate::StatusSeverity::Warning,
            );
            return;
        }
        let Some(state_path) = self
            .state_path
            .clone()
            .filter(|_| self.persistence_available)
        else {
            let result = export(&self.store);
            self.report_export_result(artifact, result);
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("mailswiftsync-report-export".into())
            .spawn(move || {
                let result = crate::core::StateStore::open_readonly(&state_path)
                    .map_err(|error| format!("could not open the ledger for export: {error}"))
                    .and_then(|store| export(&store));
                let _ = sender.send((artifact, result));
            });
        match spawned {
            Ok(_) => {
                self.report_export_receiver = Some(receiver);
                self.set_status(
                    format!("Exporting {artifact}…"),
                    crate::StatusSeverity::Info,
                );
            }
            Err(error) => self.report_export_result(
                artifact,
                Err(format!("could not start the export worker: {error}")),
            ),
        }
    }

    pub(crate) fn export_project_json(&mut self) {
        let Some((project_id, path)) = self.choose_report_export_path(
            "Project JSON",
            "mailswiftsync-project-report.json",
            "Report export cancelled.",
        ) else {
            return;
        };
        self.start_report_export("Project JSON", move |store| {
            reports::operator::export_project_json(store, &project_id, &path)
        });
    }

    pub(crate) fn export_post_migration_report(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        // Build first so a refused report fails before the operator picks a path.
        let report = reports::operator::build_post_migration_report_json(&self.store, project_id)?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-post-migration-report.json")
            .save_file()
            .ok_or("Report export cancelled.")?;
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    /// Export the customer-safe proof artifact. Unlike the operator JSON
    /// report, this intentionally omits project IDs, endpoints, plan
    /// snapshots, credential references, executable paths, and diagnostic
    /// details that may reveal internal topology.
    pub(crate) fn export_customer_proof(&mut self) {
        let Some((project_id, path)) = self.choose_report_export_path(
            "Customer proof",
            "mailswiftsync-customer-proof.json",
            "Customer proof export cancelled.",
        ) else {
            return;
        };
        let branding = self.branding.clone();
        self.start_report_export("Customer proof", move |store| {
            reports::customer::export_from_store(store, &project_id, &path, &branding)
        });
    }

    pub(crate) fn export_support_bundle_dialog(&self) -> Result<(), String> {
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-support-bundle.json")
            .save_file()
            .ok_or("Support-bundle export cancelled.")?;
        let state_path = persistent_state_path()?;
        export_support_bundle(&state_path, &path)
    }

    pub(crate) fn export_project_health(&mut self) {
        let Some((project_id, path)) = self.choose_report_export_path(
            "Project health export",
            "mailswiftsync-project-health.json",
            "Health export cancelled.",
        ) else {
            return;
        };
        self.start_report_export("Project health export", move |store| {
            reports::operator::export_health(store, &project_id, &path)
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::App;
    use std::time::{Duration, Instant};

    #[test]
    fn report_exports_run_off_the_ui_thread_on_their_own_connection() {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-report-export-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        let path = directory.join("state.db");
        let mut app = App::from_state_path(Some(&path));
        assert!(app.persistence_available);
        let project = app
            .store
            .create_project("Export project", "source.example:993", "dest.example:993")
            .unwrap();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        app.start_report_export("Project JSON", move |store| {
            // Block until the test has observed the in-flight state.
            gate.recv().map_err(|error| error.to_string())?;
            store
                .project_export_reader(&project.id)
                .map_err(|error| error.to_string())?
                .map(drop)
                .ok_or_else(|| "project missing from the export connection".to_owned())
        });
        assert!(app.background_work_pending());
        // A second export waits for the first instead of racing it.
        app.start_report_export("Customer proof", |_| Ok(()));
        assert!(app.status.text.contains("still running"));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.report_export_receiver.is_some() {
            assert!(Instant::now() < deadline, "export worker never answered");
            std::thread::sleep(Duration::from_millis(5));
            app.poll();
        }
        assert!(
            app.status
                .text
                .contains("Project JSON exported successfully"),
            "{}",
            app.status.text
        );
        drop(app);
        let _ = std::fs::remove_dir_all(directory);
    }
}
