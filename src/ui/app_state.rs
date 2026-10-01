//! Application-state helpers used by the workspace UI.

use crate::controller::{BatchActionPlan, BatchConfirmationIdentity};
use crate::*;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialDeleteTarget {
    Password { source: bool },
    OAuthRefresh { source: bool },
}

pub(crate) struct ManualOAuthRefreshResult {
    pub(crate) source: bool,
    pub(crate) marker: String,
    pub(crate) result: Result<
        (
            crate::migration_plan::OAuthRefreshOutcome,
            crate::credentials::SecretString,
        ),
        String,
    >,
}

pub(crate) struct OAuthAuthorizationResult {
    pub(crate) source: bool,
    pub(crate) marker: String,
    pub(crate) provider: String,
    pub(crate) tenant: String,
    pub(crate) mailbox: String,
    pub(crate) checked_at: String,
    pub(crate) stored: bool,
    pub(crate) refresh_tested: bool,
    pub(crate) imap_authenticated: bool,
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OAuthAuthorizationStage {
    WaitingForBrowser,
    ExchangingCode,
    TestingRefresh,
    VerifyingImap,
}

pub(crate) enum OAuthAuthorizationMessage {
    Progress(OAuthAuthorizationStage),
    Finished(OAuthAuthorizationResult),
}

/// The application's full in-memory state. This struct lives in `ui` (rather
/// than the crate root) because it is fundamentally UI/controller shared
/// state: the fields are read and mutated across `ui::*` render code,
/// `controller::*` admission/completion logic, and `bootstrap`, which
/// constructs it. Fields are `pub(crate)` rather than private because that
/// crate-wide access is exactly what callers outside this module already
/// need.
pub(crate) struct App {
    pub(crate) form: Form,
    pub(crate) output: BoundedLineBuffer,
    pub(crate) receiver: Option<Receiver<Event>>,
    pub(crate) status: StatusMessage,
    pub(crate) preview: bool,
    pub(crate) bulk_jobs: Vec<BulkJob>,
    pub(crate) settings_open: bool,
    pub(crate) bulk_search: String,
    pub(crate) bulk_state_filter: String,
    pub(crate) bulk_selected_ids: HashSet<String>,
    /// Compact representation for selecting the entire loaded queue. In this
    /// mode `bulk_selected_ids` is not populated; it stores only exclusions.
    pub(crate) bulk_all_selected: bool,
    /// The selection inspector is operator-opened; it never consumes table
    /// width merely because the queue has selected rows.
    pub(crate) bulk_inspector_open: bool,
    /// Set by the shell for the current frame so the mailbox page knows
    /// whether the inspector is already being rendered in a right panel.
    pub(crate) bulk_inspector_side_panel: bool,
    /// Reused filtered-row index storage. Large batch views must not allocate
    /// a fresh index vector on every repaint.
    pub(crate) bulk_visible_indices: Vec<usize>,
    /// Per-frame selection projection shared by the drawer and page counts.
    pub(crate) bulk_selection_view: crate::ui::batch::SelectionView,
    /// Rebuild the selected-row projection only after queue/state/selection changes.
    pub(crate) bulk_selection_view_dirty: bool,
    /// Lowercase searchable mailbox fields, rebuilt only when queue rows are
    /// imported or otherwise structurally changed.
    pub(crate) bulk_search_values: Vec<String>,
    /// State membership is maintained incrementally so a state-only filter
    /// does not scan every mailbox after each worker transition.
    pub(crate) bulk_state_indices: HashMap<String, HashSet<usize>>,
    /// Search matches are independent of durable state. Keeping this index
    /// separate means a worker state transition only intersects the changed
    /// state bucket instead of rescanning every searchable mailbox.
    pub(crate) bulk_search_match_indices: Vec<usize>,
    pub(crate) bulk_search_matches_valid: bool,
    pub(crate) bulk_filter_cache_search: String,
    pub(crate) bulk_filter_cache_state: String,
    pub(crate) bulk_filter_cache_generation: u64,
    pub(crate) bulk_jobs_generation: u64,
    pub(crate) bulk_message: String,
    /// OS-keyring IDs typed on the Mailboxes page for rows without credentials.
    pub(crate) bulk_source_keyring_apply: String,
    pub(crate) bulk_destination_keyring_apply: String,
    pub(crate) advanced_open: bool,
    pub(crate) engine_open: bool,
    pub(crate) store: core::StateStore,
    /// Persistent ledger path used to place interrupted verification stages
    /// beside the private state database. None means the session is already
    /// in-memory and cannot promise restartable staging.
    pub(crate) state_path: Option<PathBuf>,
    /// Held for the lifetime of the application. An advisory OS lock is
    /// released automatically if the process crashes, so a later instance
    /// can safely perform orphan recovery without killing a live sibling.
    pub(crate) _instance_lock: Option<InstanceLock>,
    /// Startup could not prove that every recorded process identity was
    /// gone or owned by this application. No new execution is allowed until
    /// the operator confirms the host has been checked.
    pub(crate) process_review_required: bool,
    pub(crate) persistence_available: bool,
    pub(crate) profile_available: bool,
    /// The project currently selected by the operator for views and exports.
    /// This is deliberately separate from `active_run`, which is execution
    /// ownership and must never be inferred from UI selection.
    pub(crate) selected_project_id: Option<String>,
    /// The project browser normally keeps a compact recent index. This flag
    /// is set only when the operator explicitly asks for the searchable full
    /// project index.
    pub(crate) ui_all_projects_loaded: bool,
    /// A selected project that is not the current editable execution context
    /// is a historical read-only view. Keeping this explicit prevents the
    /// header/project browser from implying that the visible plan is safe to
    /// mutate or execute for that project.
    pub(crate) workspace_read_only: bool,
    pub(crate) projects_open: bool,
    pub(crate) project_search: String,
    /// Cached project-browser matches. The browser is repainted frequently;
    /// filtering by index avoids cloning every project on every frame.
    pub(crate) project_filter_query: String,
    pub(crate) project_filter_source_revision: u64,
    pub(crate) project_visible_indices: Vec<usize>,
    pub(crate) project_id: Option<String>,
    pub(crate) job_id: Option<String>,
    /// Process-local credential material used by the last successful dry
    /// preflight. Missing after restart intentionally requires revalidation.
    pub(crate) preflight_credential_fingerprint: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) active_run: Option<ActiveRunContext>,
    /// Optional operator-selected diagnostic transcript. Disabled by default
    /// because engine output may contain mailbox and folder metadata.
    pub(crate) diagnostic_logger: Option<Arc<DiagnosticLogger>>,
    /// Non-secret plan values captured for the active execution. The egui
    /// form remains visible while a run is active, but edits must not mutate
    /// the plan presented to the operator or the next retry.
    pub(crate) locked_profile: Option<Profile>,
    pub(crate) cancel_requested: Option<Arc<AtomicBool>>,
    pub(crate) bulk_project_id: Option<String>,
    pub(crate) bulk_job_ids: Vec<String>,
    pub(crate) bulk_job_index_by_id: HashMap<String, usize>,
    /// Process-local credential material from the last successful dry
    /// validation for each durable queue row. Restored queues start empty.
    pub(crate) bulk_preflight_credential_fingerprints: Vec<Option<String>>,
    pub(crate) preflight: Vec<(String, String, bool)>,
    pub(crate) capability_receiver: Option<Receiver<CapabilityProbeResult>>,
    pub(crate) capability_probe_request_id: Option<String>,
    pub(crate) capability_probe_fingerprint: Option<String>,
    pub(crate) capability_observation_fingerprint: Option<String>,
    /// Last per-frame readiness staleness check; see `poll`.
    pub(crate) capability_staleness_checked_at: Option<std::time::Instant>,
    /// Fresh authentication completed immediately before an IMAPS live run.
    /// Both digests are captured at probe launch and must still match when
    /// the run is admitted.
    pub(crate) live_auth_receiver: Option<Receiver<Result<LiveAuthProof, String>>>,
    pub(crate) manual_oauth_refresh_receiver: Option<Receiver<ManualOAuthRefreshResult>>,
    pub(crate) keyring_operation_receiver:
        Option<Receiver<crate::ui::keyring_ops::KeyringOperationResult>>,
    pub(crate) oauth_authorization_receiver: Option<Receiver<OAuthAuthorizationMessage>>,
    pub(crate) oauth_authorization_cancel: Option<Arc<AtomicBool>>,
    pub(crate) oauth_authorization_stage: Option<OAuthAuthorizationStage>,
    pub(crate) oauth_authorization_provider: String,
    pub(crate) oauth_authorization_tenant: String,
    pub(crate) oauth_authorization_client_id: String,
    pub(crate) oauth_authorization_client_secret: SecretString,
    pub(crate) oauth_authorization_login_hint: String,
    pub(crate) oauth_authorization_source: bool,
    pub(crate) oauth_authorization_redirect_uri: String,
    pub(crate) oauth_authorization_result: Option<OAuthAuthorizationResult>,
    /// Loading an OS-keyring credential can involve IPC and must not block an
    /// egui frame. The cloned form is returned only after the worker has
    /// completed the load.
    pub(crate) start_credentials_receiver: Option<Receiver<Result<Form, String>>>,
    pub(crate) start_credentials_plan: Option<String>,
    pub(crate) credentials_ready_for_start: bool,
    pub(crate) live_auth_proof: Option<LiveAuthProof>,
    pub(crate) source_capabilities: Option<core::ServerCapabilities>,
    pub(crate) destination_capabilities: Option<core::ServerCapabilities>,
    pub(crate) live_confirm_open: bool,
    pub(crate) live_confirm_focus_requested: bool,
    pub(crate) live_confirmed: bool,
    pub(crate) live_confirmation_plan: Option<String>,
    pub(crate) durability_error: bool,
    pub(crate) durability_recovery_pending: bool,
    pub(crate) stop_confirm_open: bool,
    pub(crate) stop_confirm_focus_requested: bool,
    /// Closing the desktop would terminate its controller and interrupt the
    /// engine; require the operator to acknowledge that boundary first.
    pub(crate) close_during_run_confirm_open: bool,
    pub(crate) keyring_open: bool,
    pub(crate) credential_delete_confirmation: Option<CredentialDeleteTarget>,
    pub(crate) credential_delete_focus_requested: bool,
    /// Session-only editor buffers for an automatic OAuth refresh
    /// configuration. Populated by the operator, then written into the OS
    /// keyring by `store_oauth_refresh_config`; never persisted to the
    /// profile or the durable ledger.
    pub(crate) oauth_refresh_editor_endpoint: String,
    pub(crate) oauth_refresh_editor_client_id: String,
    pub(crate) oauth_refresh_editor_client_secret: SecretString,
    pub(crate) oauth_refresh_editor_refresh_token: SecretString,
    pub(crate) active_view: WorkspaceView,
    pub(crate) pending_evidence: Option<core::MailboxEvidence>,
    pub(crate) pending_mismatches: Vec<core::MessageMismatch>,
    pub(crate) pending_batch_evidence: HashMap<String, core::MailboxEvidence>,
    pub(crate) pending_batch_mismatches: HashMap<String, Vec<core::MessageMismatch>>,
    pub(crate) pending_checkpoint: Option<String>,
    pub(crate) pending_batch_checkpoints: HashMap<String, String>,
    /// Execution diagnostics that could not yet be committed. These remain
    /// in memory and are retried before later terminal events are handled.
    pub(crate) pending_db_events: Vec<PendingDbEvent>,
    /// Events deferred because their durable predecessor could not be
    /// committed. Keeping them here prevents a child completion from being
    /// silently lost when SQLite is temporarily unavailable.
    pub(crate) deferred_events: VecDeque<Event>,
    pub(crate) run_started_at: Option<std::time::Instant>,
    pub(crate) dark_mode: bool,
    pub(crate) theme: ThemeKind,
    pub(crate) ui_scale: f32,
    pub(crate) language: UiLanguage,
    /// Operator/agency name and contact line applied to customer-proof
    /// exports. Independent of the migration plan/profile; see
    /// `branding::OperatorBranding`.
    pub(crate) branding: crate::branding::OperatorBranding,
    pub(crate) bulk_live_confirm_open: bool,
    pub(crate) bulk_live_confirm_focus_requested: bool,
    pub(crate) bulk_live_confirmed: bool,
    /// Explicit acknowledgement, inside the open confirmation, that the
    /// plan may remove destination-only state. Reset whenever it closes.
    pub(crate) bulk_destination_loss_acknowledged: bool,
    /// Case-folded destination collisions are ambiguous for generic IMAP
    /// endpoints and require acknowledgement inside the live confirmation.
    pub(crate) bulk_destination_case_acknowledged: bool,
    pub(crate) live_destination_loss_acknowledged: bool,
    pub(crate) bulk_confirmation_summary: Option<BatchActionPlan>,
    /// Proof of what the operator saw when they opened the confirmation dialog.
    /// Must match exactly when they click "Confirm" or the dialog re-opens.
    pub(crate) bulk_confirmation_identity: Option<BatchConfirmationIdentity>,
    pub(crate) bulk_summary: Option<(u64, BulkQueueSummary)>,
    pub(crate) bulk_mode: BatchExecutionMode,
    pub(crate) bulk_clear_confirm_open: bool,
    pub(crate) pending_bulk_import: Option<std::path::PathBuf>,
    pub(crate) pending_plaintext_import: Option<std::path::PathBuf>,
    pub(crate) bulk_plaintext_import_acknowledged: bool,
    pub(crate) plaintext_import_dialog_acknowledged: bool,
    pub(crate) pending_sheet_import: Option<PendingSheetImport>,
    pub(crate) bulk_sheet_index: usize,
    pub(crate) bulk_import_receiver: Option<Receiver<Result<BulkImportResult, String>>>,
    pub(crate) bulk_live_run: bool,
    /// Live retry scope defaults to unresolved rows and is process-local UI
    /// state; durable child/run IDs remain the execution identity.
    pub(crate) bulk_retry_scope: BulkRetryScope,
    pub(crate) verification_exception_operator: String,
    pub(crate) verification_exception_reason: String,
    pub(crate) verification_search: String,
    pub(crate) verification_filter: String,
    pub(crate) verification_attention_reason: Option<core::AttentionReason>,
    pub(crate) verification_visible_indices: Vec<usize>,
    pub(crate) verification_filter_cache_search: String,
    pub(crate) verification_filter_cache_state: String,
    pub(crate) verification_filter_cache_reason: Option<core::AttentionReason>,
    pub(crate) verification_filter_cache_offset: u32,
    pub(crate) verification_filter_cache_revision: Option<i64>,
    pub(crate) verification_filter_cache_project: Option<String>,
    pub(crate) verification_filter_cache_rows: usize,
    pub(crate) activity_show_all: bool,
    pub(crate) activity_search: String,
    pub(crate) activity_status_filter: String,
    pub(crate) reopen_reason: String,
    /// Database-backed UI read model. Rendering consumes this cache instead
    /// of issuing SQLite queries on every egui repaint.
    pub(crate) ui_snapshot: WorkspaceSnapshot,
    /// GUI-only read-model worker; `None` keeps the synchronous refresh path.
    pub(crate) snapshot_worker: Option<crate::ui::snapshot_worker::SnapshotWorker>,
    pub(crate) historical_mailbox_offset: u32,
    pub(crate) historical_mailbox_cursor: Option<i64>,
    pub(crate) historical_mailbox_cursor_stack: Vec<Option<i64>>,
    pub(crate) verification_offset: u32,
    pub(crate) verification_cursor: Option<i64>,
    pub(crate) verification_cursor_stack: Vec<Option<i64>>,
    pub(crate) source_provider: ProviderPreset,
    pub(crate) destination_provider: ProviderPreset,
}
impl Default for App {
    fn default() -> Self {
        Self::from_state_path(None)
    }
}

/// Pre-live readiness and quota checks remain in `imap_probe`; this root module
/// only wires the shared result into the application controller.
impl App {
    pub(crate) fn bulk_selection_count(&self) -> usize {
        if self.bulk_all_selected {
            self.bulk_jobs
                .len()
                .saturating_sub(self.bulk_selected_ids.len())
        } else {
            self.bulk_selected_ids.len()
        }
    }

    pub(crate) fn bulk_selection_is_empty(&self) -> bool {
        self.bulk_selection_count() == 0
    }

    pub(crate) fn bulk_is_selected(&self, job_id: &str) -> bool {
        if self.bulk_all_selected {
            !self.bulk_selected_ids.contains(job_id)
        } else {
            self.bulk_selected_ids.contains(job_id)
        }
    }

    pub(crate) fn clear_bulk_selection(&mut self) {
        self.bulk_selected_ids.clear();
        self.bulk_all_selected = false;
        self.bulk_selection_view_dirty = true;
    }

    pub(crate) fn select_all_bulk_rows(&mut self) {
        self.bulk_selected_ids.clear();
        self.bulk_all_selected = true;
        self.bulk_selection_view_dirty = true;
    }

    pub(crate) fn bulk_selection_scope(&self) -> crate::controller::SelectionScope {
        if self.bulk_all_selected {
            crate::controller::SelectionScope::AllMatching {
                filter: String::new(),
                state: "all".to_owned(),
                excluded_ids: self.bulk_selected_ids.clone(),
            }
        } else {
            crate::controller::SelectionScope::Explicit(self.bulk_selected_ids.clone())
        }
    }

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
        ThemeColors::for_theme(self.theme, self.dark_mode)
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

    pub(crate) fn snapshot_refresh_options(&self) -> crate::ui::OwnedRefreshOptions {
        crate::ui::OwnedRefreshOptions {
            active_project_id: self.active_project_id().map(str::to_owned),
            all_projects_loaded: self.ui_all_projects_loaded,
            mailbox_offset: if self.workspace_read_only {
                self.historical_mailbox_offset
            } else {
                0
            },
            mailbox_cursor: if self.workspace_read_only {
                self.historical_mailbox_cursor
            } else {
                None
            },
            verification_offset: self.verification_offset,
            verification_cursor: self.verification_cursor,
            verification_attention_reason: self.verification_attention_reason,
            load_report: matches!(
                self.active_view,
                WorkspaceView::Overview
                    | WorkspaceView::Mailboxes
                    | WorkspaceView::Activity
                    | WorkspaceView::Verification
            ),
            load_runs: self.active_view == WorkspaceView::Activity,
        }
    }

    /// Move read-model refreshes onto a worker with its own read-only
    /// connection, so SQLite contention cannot stall egui frames. Returns
    /// whether the worker started; without one, refresh stays synchronous.
    pub(crate) fn enable_snapshot_worker(&mut self) -> bool {
        if self.snapshot_worker.is_some() {
            return true;
        }
        let Some(path) = self
            .state_path
            .clone()
            .filter(|_| self.persistence_available)
        else {
            return false;
        };
        match crate::ui::snapshot_worker::SnapshotWorker::spawn(path) {
            Ok(worker) => {
                self.snapshot_worker = Some(worker);
                true
            }
            Err(error) => {
                push_visible_output(
                    &mut self.output,
                    format!(
                        "[read-model] worker unavailable; refreshing on the UI thread: {error}"
                    ),
                );
                false
            }
        }
    }

    /// Refresh the database-backed UI read model at a low frequency. egui may
    /// repaint many times per second while a process is producing output;
    /// those repaints must not turn into repeated SQLite reads.
    pub(crate) fn refresh_ui_snapshot(&mut self) {
        let options = self.snapshot_refresh_options();
        if let Some(worker) = self.snapshot_worker.as_mut() {
            match worker.poll(&mut self.ui_snapshot, &options) {
                crate::ui::snapshot_worker::SnapshotPoll::Failed(error) => {
                    self.snapshot_worker = None;
                    push_visible_output(
                        &mut self.output,
                        format!("[read-model] {error}; refreshing on the UI thread"),
                    );
                }
                _ => return,
            }
        }
        self.ui_snapshot.refresh(&self.store, options.borrowed());
    }

    pub(crate) fn refresh_ui_snapshot_now(&mut self) {
        match self.snapshot_worker.as_mut() {
            Some(worker) => worker.request_invalidation(),
            None => self.ui_snapshot.invalidate(),
        }
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
        self.historical_mailbox_cursor = None;
        self.historical_mailbox_cursor_stack.clear();
        self.verification_offset = 0;
        self.verification_cursor = None;
        self.verification_cursor_stack.clear();
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

    /// Any background channel that `poll` must keep draining.
    pub(crate) fn background_work_pending(&self) -> bool {
        self.running()
            || self.capability_receiver.is_some()
            || self.live_auth_receiver.is_some()
            || self.manual_oauth_refresh_receiver.is_some()
            || self
                .snapshot_worker
                .as_ref()
                .is_some_and(crate::ui::snapshot_worker::SnapshotWorker::in_flight)
            || self.keyring_operation_receiver.is_some()
            || self.oauth_authorization_receiver.is_some()
            || self.start_credentials_receiver.is_some()
            || self.bulk_import_receiver.is_some()
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
            || !self
                .form
                .profile
                .source_oauth_refresh_credential_id
                .trim()
                .is_empty()
            || !self
                .form
                .profile
                .destination_oauth_refresh_credential_id
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
