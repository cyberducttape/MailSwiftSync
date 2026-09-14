//! Application-state helpers used by the workspace UI.

use crate::*;

/// Pre-live readiness and quota checks remain in `imap_probe`; this root module
/// only wires the shared result into the application controller.
impl App {
    pub(crate) fn set_status(&mut self, message: impl Into<String>, severity: StatusSeverity) {
        self.status = StatusMessage::new(message, severity);
    }

    pub(crate) fn support_summary(&self) -> String {
        format!(
            "MailSwiftSync support summary\nRun active: {}\nStatus severity: {:?}\nRetained diagnostic lines: {}\n\nDetailed engine output was intentionally omitted; use the sanitized support-bundle command for durable diagnostics.",
            self.running(),
            self.status.severity,
            self.output.len(),
        )
    }

    pub(crate) fn theme_colors(&self) -> ThemeColors {
        if self.dark_mode {
            ThemeColors::dark()
        } else {
            ThemeColors::light()
        }
    }

    pub(crate) fn cached_report_mailbox(
        &self,
        job_id: &str,
    ) -> Option<&core::ReportMailboxSnapshot> {
        self.ui_snapshot
            .verification_rows
            .iter()
            .find(|mailbox| mailbox.job.id == job_id)
    }

    /// Refresh the database-backed UI read model at a low frequency. egui may
    /// repaint many times per second while a process is producing output;
    /// those repaints must not turn into repeated SQLite reads.
    pub(crate) fn refresh_ui_snapshot(&mut self) {
        let project_id = self.active_project_id().map(str::to_owned);
        self.ui_snapshot.refresh(
            &self.store,
            WorkspaceRefreshOptions {
                active_project_id: project_id.as_deref(),
                all_projects_loaded: self.ui_all_projects_loaded,
                mailbox_offset: if self.workspace_read_only {
                    self.historical_mailbox_offset
                } else {
                    0
                },
                verification_offset: self.verification_offset,
                load_report: matches!(
                    self.active_view,
                    WorkspaceView::Activity | WorkspaceView::Verification
                ),
                load_runs: self.active_view == WorkspaceView::Activity,
            },
        );
    }

    pub(crate) fn refresh_ui_snapshot_now(&mut self) {
        self.ui_snapshot.invalidate();
        self.refresh_ui_snapshot();
    }

    pub(crate) fn apply_provider_preset(&mut self, source: bool, preset: ProviderPreset) {
        let defaults = preset.defaults();
        let (host, port, tls, auth) = if source {
            (
                &mut self.form.profile.source_host,
                &mut self.form.profile.source_port,
                &mut self.form.profile.source_tls,
                &mut self.form.profile.source_auth,
            )
        } else {
            (
                &mut self.form.profile.destination_host,
                &mut self.form.profile.destination_port,
                &mut self.form.profile.destination_tls,
                &mut self.form.profile.destination_auth,
            )
        };
        *host = defaults.host.into();
        *port = defaults.port.into();
        *tls = defaults.tls.into();
        *auth = defaults.auth.into();
    }

    pub(crate) fn current_editable_project_id(&self) -> Option<&str> {
        self.active_run
            .as_ref()
            .map(|run| run.project_id.as_str())
            .or(self.bulk_project_id.as_deref())
            .or(self.project_id.as_deref())
    }

    pub(crate) fn select_workspace_project(&mut self, project_id: String) {
        if self.running() {
            self.set_status(
                "Project switching is disabled while a migration is running.",
                StatusSeverity::Warning,
            );
            return;
        }
        let editable_id = self.current_editable_project_id().map(str::to_owned);
        self.workspace_read_only = editable_id.as_deref() != Some(project_id.as_str());
        self.selected_project_id = Some(project_id);
        self.historical_mailbox_offset = 0;
        self.verification_offset = 0;
        self.active_view = WorkspaceView::Overview;
        self.capability_receiver = None;
        self.capability_probe_request_id = None;
        self.capability_probe_fingerprint = None;
        self.capability_observation_fingerprint = None;
        // Selection changes must invalidate the throttled read-model refresh
        // immediately. Otherwise the header can render the new selection
        // while still holding the previous project's cached rows.
        self.refresh_ui_snapshot_now();
        if self.workspace_read_only {
            self.preflight.clear();
            self.source_capabilities = None;
            self.destination_capabilities = None;
            self.set_status(
                "Viewing a historical project read-only. Start a new migration to edit or execute a plan.",
                StatusSeverity::Info,
            );
        }
    }

    pub(crate) fn active_project_id(&self) -> Option<&str> {
        preferred_project_id(
            self.active_run.as_ref().map(|run| run.project_id.as_str()),
            self.selected_project_id.as_deref(),
            self.bulk_project_id.as_deref(),
            self.project_id.as_deref(),
        )
    }

    pub(crate) fn running(&self) -> bool {
        self.receiver.is_some()
    }
    pub(crate) fn report_store_error<E: Display>(
        &mut self,
        operation: &str,
        result: Result<(), E>,
    ) {
        if let Err(error) = result {
            self.durability_error = true;
            push_visible_output(
                &mut self.output,
                format!("[durability] {operation} failed: {error}"),
            );
            self.set_status(
                format!("Durability error: {operation}"),
                StatusSeverity::Error,
            );
        }
    }

    pub(crate) fn report_export_result(&mut self, artifact: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.set_status(
                    format!("{artifact} exported successfully."),
                    StatusSeverity::Success,
                );
                push_visible_output(&mut self.output, self.status.text.clone());
            }
            Err(error) => {
                self.set_status(
                    format!("{artifact} was not exported: {error}"),
                    StatusSeverity::Error,
                );
                push_visible_output(&mut self.output, format!("[export] {}", self.status.text));
            }
        }
    }

    pub(crate) fn start_requires_keyring_load(&self) -> bool {
        !self.form.profile.source_credential_id.trim().is_empty()
            || !self
                .form
                .profile
                .destination_credential_id
                .trim()
                .is_empty()
    }

    /// Cheap marker used only to discard a keyring result if the operator
    /// edits the form while the IPC call is in flight. The authoritative plan
    /// fingerprint is still computed later, after the credentials are loaded;
    /// this marker deliberately avoids hashing large executable/CA files on
    /// the UI thread.
    pub(crate) fn start_credentials_plan_marker(&self) -> String {
        format!(
            "dry-run={}\n{}",
            self.form.dry_run,
            toml::to_string(&self.form.profile).unwrap_or_default()
        )
    }
}
