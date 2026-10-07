use crate::*;

/// Neither the durable ledger nor the temporary in-memory fallback could be
/// opened. Startup stops here: without a state store no plan, claim, or
/// evidence can be recorded, so no migration may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BootstrapFailure {
    /// Why durable state was unavailable.
    pub(crate) durable: String,
    /// The SQLite error from opening the temporary in-memory store.
    pub(crate) temporary: String,
}

impl BootstrapFailure {
    pub(crate) const SUMMARY: &'static str = "MailSwiftSync could not initialize either durable or temporary state. No migration can be started.";
}

impl std::fmt::Display for BootstrapFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}\nDurable state: {}\nTemporary state: {}",
            Self::SUMMARY,
            self.durable,
            self.temporary
        )
    }
}

/// Open the non-durable fallback after durable state failed for `reason`.
fn temporary_state_store(
    reason: String,
    open: impl FnOnce() -> rusqlite::Result<core::StateStore>,
) -> Result<(core::StateStore, Option<String>), BootstrapFailure> {
    let warning = format!("Persistent SQLite state unavailable: {reason}");
    match open() {
        Ok(store) => Ok((store, Some(warning))),
        Err(error) => Err(BootstrapFailure {
            durable: warning,
            temporary: format!("SQLite in-memory store unavailable: {error}"),
        }),
    }
}

impl App {
    /// Test convenience: tests own their state paths, so a bootstrap
    /// failure there is a test-environment bug worth a loud panic.
    #[cfg(test)]
    pub(crate) fn from_state_path(state_override: Option<&std::path::Path>) -> Self {
        Self::try_from_state_path(state_override).unwrap_or_else(|failure| panic!("{failure}"))
    }

    /// Construct the application against an explicit ledger path. Headless
    /// callers use this to avoid process-global environment mutation while
    /// retaining the same recovery and durable-state behavior as the GUI.
    pub(crate) fn try_from_state_path(
        state_override: Option<&std::path::Path>,
    ) -> Result<Self, BootstrapFailure> {
        let appearance = AppearancePreferences::load();
        let state_path_result = match state_override {
            Some(path) => Ok(path.to_owned()),
            None => persistent_state_path(),
        };
        let path_error = state_path_result.as_ref().err().cloned();
        let state_path = state_path_result.ok();
        let state_directory_error =
            state_path
                .as_ref()
                .and_then(|path| path.parent())
                .and_then(|parent| {
                    // Security boundary: only restrict permissions on directories we create.
                    // If the parent already exists, verify its permissions rather than mutating them.
                    // This prevents privilege-escalation attacks where a malicious state_path
                    // could cause MailSwiftSync to chmod /tmp, /var/lib, or other system directories.
                    match std::fs::symlink_metadata(parent) {
                        Ok(_) => {
                            // Establish the no-follow ownership and permission
                            // boundary before probing writability; the probe
                            // creates a file below this directory.
                            match crate::credentials::verify_private_directory(parent)
                                .and_then(|_| crate::credentials::verify_directory_writable(parent))
                            {
                                Ok(()) => None,
                                Err(e) => Some(format!(
                                    "State directory exists but is not writable: {e}"
                                )),
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Parent does not exist. Create it and restrict permissions.
                            crate::credentials::ensure_private_directory(parent)
                                .err()
                                .map(|error| {
                                    format!("Could not create and secure persistent state directory: {error}")
                                })
                        }
                        Err(e) => Some(format!("Could not access state directory parent: {e}")),
                    }
                });
        let instance_lock = state_path
            .as_ref()
            .map(|path| acquire_instance_lock(path.as_path()));
        let (store, mut persistence_warning) = match (state_path.as_ref(), instance_lock.as_ref()) {
            (Some(path), Some(Ok(_))) => match core::StateStore::open(path) {
                Ok(store) => (store, None),
                Err(error) => {
                    temporary_state_store(error.to_string(), core::StateStore::in_memory)?
                }
            },
            (_, Some(Err(error))) => {
                temporary_state_store(error.to_string(), core::StateStore::in_memory)?
            }
            _ => temporary_state_store(
                path_error
                    .as_deref()
                    .unwrap_or("no durable state path is available")
                    .to_owned(),
                core::StateStore::in_memory,
            )?,
        };
        if let Some(error) = state_directory_error {
            persistence_warning.get_or_insert(error);
        }
        let (recovered, orphaned, unverified_processes) = if persistence_warning.is_none() {
            let processes = match store.active_processes() {
                Ok(processes) => processes,
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent SQLite recovery unavailable; execution is blocked: {error}"
                    ));
                    Vec::new()
                }
            };
            if persistence_warning.is_some() {
                (0, 0, Vec::new())
            } else {
                let mut unverified = Vec::new();
                for process in &processes {
                    if process.pid > 0 && recorded_process_matches(process) {
                        terminate_recorded_process_group(process);
                        if recorded_process_matches(process) {
                            unverified.push(process.clone());
                        }
                    } else if recorded_process_is_gone(process) {
                        // The registered process exited before startup
                        // recovery could inspect it; do not preserve a dead
                        // PID as if it were an unverified live owner.
                    } else {
                        unverified.push(process.clone());
                    }
                }
                match store.recover_abandoned_jobs_preserving(&unverified) {
                    Ok(recovered) => (recovered, processes.len(), unverified),
                    Err(error) => {
                        persistence_warning = Some(format!(
                            "Persistent SQLite recovery failed; execution is blocked: {error}"
                        ));
                        (0, 0, Vec::new())
                    }
                }
            }
        } else {
            (0, 0, Vec::new())
        };
        // The instance lock and completed durable-process reconciliation make
        // immediate cleanup safe when there are no unverified owners. If an
        // owner remains ambiguous, retain young directories and apply only
        // the seven-day fallback age policy.
        if persistence_warning.is_none() {
            if unverified_processes.is_empty() {
                cleanup_reconciled_secret_directories(&secret_runtime_base());
            } else {
                cleanup_stale_secret_directories(&secret_runtime_base());
            }
        }
        let mut initial_output_lines = persistence_warning.clone().map_or_else(
            || vec!["Ready. Start with Preflight against a test destination mailbox.".into()],
            |warning| vec![warning, "WARNING: this session is not durable.".into()],
        );
        if orphaned > 0 {
            initial_output_lines.push(format!(
                "Startup found {orphaned} recorded migration process(es); verified identities were terminated before recovery."
            ));
        }
        let unverified_process_count = unverified_processes.len();
        if unverified_process_count > 0 {
            initial_output_lines.push(format!(
                "{unverified_process_count} recorded process identity(ies) could not be verified and were not signalled; review the affected jobs before retrying."
            ));
            initial_output_lines.push(
                "Immediate secret cleanup was deferred; directories older than seven days remain eligible for age-based cleanup."
                    .into(),
            );
        }
        if recovered > 0 {
            initial_output_lines.push(format!(
                "Recovered {recovered} interrupted job(s) into Attention for review."
            ));
        }
        let mut initial_output: BoundedLineBuffer = BoundedLineBuffer::new();
        for line in initial_output_lines {
            push_visible_output(&mut initial_output, line);
        }
        let (mut form, profile_warning) = match Form::load() {
            Ok(form) => (form, None),
            Err(error) => (
                Form::default(),
                Some(format!(
                    "Saved migration profile is unavailable; execution is blocked: {error}"
                )),
            ),
        };
        if let Some(warning) = profile_warning.as_ref() {
            push_visible_output(&mut initial_output, warning.clone());
            push_visible_output(
                &mut initial_output,
                "History and reports remain available; repair the profile before execution.".into(),
            );
        }
        if let Some(warning) = persistence_warning.as_ref()
            && !initial_output.iter().any(|line| line == warning)
        {
            initial_output.push_front_bounded(
                truncate_utf8(warning, MAX_DIAGNOSTIC_LINE_BYTES),
                MAX_VISIBLE_OUTPUT_LINES,
                MAX_VISIBLE_OUTPUT_BYTES,
            );
            push_visible_output(
                &mut initial_output,
                "WARNING: saved configuration must be repaired before execution.".into(),
            );
        }
        if form.profile.destination_tls.is_empty() {
            form.profile.destination_tls = default_destination_tls();
        }
        let restored_project = if persistence_warning.is_none() {
            match store.latest_project() {
                Ok(project) => project,
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent project restore failed; execution is blocked: {error}"
                    ));
                    None
                }
            }
        } else {
            None
        };
        // Batch projects now retain their real endpoints, so identify them by
        // their durable per-mailbox configuration rather than the old
        // sentinel endpoint values. This also prevents a restored batch from
        // being mistaken for the editable single-mailbox workspace.
        let restored_project_is_batch = restored_project.as_ref().is_some_and(
            |project| match store.project_has_complete_mailbox_configs(&project.id) {
                Ok(is_batch) => is_batch,
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent project classification failed; execution is blocked: {error}"
                    ));
                    false
                }
            },
        );
        let (project_id, job_id) = restored_project
            .as_ref()
            .filter(|project| {
                project.source_endpoint == form.profile.source_host
                    && project.destination_endpoint == form.profile.destination_host
                    && !restored_project_is_batch
            })
            .map(|project| {
                let job_id = match store.first_mailbox(&project.id) {
                    Ok(job_id) => job_id,
                    Err(error) => {
                        persistence_warning = Some(format!(
                            "Persistent mailbox restore failed; execution is blocked: {error}"
                        ));
                        None
                    }
                };
                if let Some(job) = &job_id {
                    match store.mailbox_identity(job) {
                        Ok(Some((source_user, destination_user, state))) => {
                            // Restore non-secret mailbox identity and make
                            // recovery state visible immediately. Passwords
                            // remain blank and must be entered again before a
                            // live run.
                            form.profile.source_user = source_user;
                            form.profile.destination_user = destination_user;
                            if state == "attention" {
                                form.dry_run = true;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            persistence_warning = Some(format!(
                                "Persistent mailbox identity restore failed; execution is blocked: {error}"
                            ));
                        }
                    }
                }
                (Some(project.id.clone()), job_id)
            })
            .unwrap_or((None, None));
        // A batch project's rows are the queue; restoring it means showing
        // that project. Rows written before queue facts were stored are
        // indexed once here, and every row's plan must still decode so a
        // corrupt queue blocks execution instead of being silently skipped.
        let restored_queue = restored_project.as_ref().and_then(|project| {
            if project.name.trim().is_empty() || !restored_project_is_batch {
                return None;
            }
            if let Err(error) = crate::controller::queue::backfill_queue_facts(&store) {
                persistence_warning = Some(format!(
                    "Persistent batch restore failed; execution is blocked: {error}"
                ));
                return None;
            }
            let decoded = store.queue_plan_scan(&project.id, |row| {
                decode_persisted_batch_profile(row.config.as_deref(), &row.id).map(|_| ())
            });
            if let Err(error) = decoded {
                persistence_warning = Some(format!("{error}; execution is blocked"));
                return None;
            }
            match store.queue_len(&project.id) {
                Ok(len) => Some((project.id.clone(), len)),
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent batch restore failed; execution is blocked: {error}"
                    ));
                    None
                }
            }
        });
        let restored_bulk_project_id = restored_queue.as_ref().map(|(id, _)| id.clone());
        let mut queue = crate::ui::queue_model::MailboxQueue::default();
        if let Some((project_id, len)) = restored_queue {
            queue.attach(project_id, len);
        }
        if let Some(warning) = persistence_warning.as_ref()
            && !initial_output.iter().any(|line| line == warning)
        {
            initial_output.push_front_bounded(
                truncate_utf8(warning, MAX_DIAGNOSTIC_LINE_BYTES),
                MAX_VISIBLE_OUTPUT_LINES,
                MAX_VISIBLE_OUTPUT_BYTES,
            );
            push_visible_output(
                &mut initial_output,
                "WARNING: durable state could not be restored; repair the ledger before execution."
                    .into(),
            );
        }
        Ok(Self {
            form,
            output: initial_output,
            receiver: None,
            status: StatusMessage::new(
                persistence_warning
                    .clone()
                    .or_else(|| profile_warning.clone())
                    .unwrap_or_else(|| "Idle".into()),
                if persistence_warning.is_some() {
                    StatusSeverity::Error
                } else if profile_warning.is_some() {
                    StatusSeverity::Warning
                } else {
                    StatusSeverity::Info
                },
            ),
            preview: false,
            queue,
            recovery: Default::default(),
            waves: Default::default(),
            settings_open: false,
            bulk_search: String::new(),
            bulk_state_filter: "all".into(),
            bulk_wave: None,
            bulk_selected_ids: HashSet::new(),
            bulk_all_selected: false,
            bulk_inspector_open: false,
            bulk_inspector_side_panel: false,
            bulk_source_keyring_apply: String::new(),
            bulk_destination_keyring_apply: String::new(),
            bulk_selection_view: Default::default(),
            bulk_selection_view_dirty: true,
            bulk_rendered_range: (0, 0),
            bulk_message: if restored_bulk_project_id.is_some() {
                "Restored durable batch queue; credentials must be entered again before validation."
                    .into()
            } else {
                // The empty Mailboxes page already explains CSV/XLSX import.
                String::new()
            },
            advanced_open: false,
            folder_mapping_source: String::new(),
            folder_mapping_destination: String::new(),
            folder_mapping_exclude: false,
            // Do not interrupt first launch with a configuration dialog. The
            // conservative Auto engine is already selected and can be
            // changed from the migration plan when the endpoints are known.
            engine_open: false,
            store,
            state_path,
            _instance_lock: instance_lock.and_then(Result::ok),
            process_review_required: unverified_process_count > 0,
            persistence_available: persistence_warning.is_none(),
            profile_available: profile_warning.is_none(),
            selected_project_id: restored_bulk_project_id
                .clone()
                .or_else(|| project_id.clone()),
            ui_all_projects_loaded: false,
            workspace_read_only: false,
            projects_open: false,
            project_search: String::new(),
            project_filter_query: String::new(),
            project_filter_source_revision: u64::MAX,
            project_visible_indices: Vec::new(),
            project_id,
            job_id,
            preflight_credential_fingerprint: None,
            run_id: None,
            active_run: None,
            diagnostic_logger: None,
            locked_profile: None,
            cancel_requested: None,
            preflight: Vec::new(),
            capability_receiver: None,
            capability_probe_request_id: None,
            capability_probe_fingerprint: None,
            capability_observation_fingerprint: None,
            capability_staleness_checked_at: None,
            live_auth_receiver: None,
            manual_oauth_refresh_receiver: None,
            keyring_operation_receiver: None,
            engine_setup_receiver: None,
            engine_setup_progress: None,
            engine_setup_status: None,
            oauth_authorization_receiver: None,
            oauth_authorization_cancel: None,
            oauth_authorization_stage: None,
            oauth_authorization_provider: "google".into(),
            oauth_authorization_tenant: String::new(),
            oauth_authorization_client_id: String::new(),
            oauth_registrations: crate::oauth_onboarding::ClientRegistrations::load(),
            oauth_authorization_client_secret: SecretString::default(),
            oauth_authorization_login_hint: String::new(),
            oauth_authorization_source: true,
            oauth_authorization_redirect_uri: String::new(),
            oauth_authorization_result: None,
            plan_oauth_connect_side: None,
            plan_oauth_prefilled_id: None,
            start_credentials_receiver: None,
            start_credentials_plan: None,
            credentials_ready_for_start: false,
            live_auth_proof: None,
            source_capabilities: None,
            destination_capabilities: None,
            live_confirm_open: false,
            live_confirm_focus_requested: false,
            live_confirmed: false,
            live_confirmation_plan: None,
            durability_error: false,
            durability_recovery_pending: false,
            stop_confirm_open: false,
            stop_confirm_focus_requested: false,
            close_during_run_confirm_open: false,
            keyring_open: false,
            credential_delete_confirmation: None,
            credential_delete_focus_requested: false,
            oauth_refresh_editor_endpoint: String::new(),
            oauth_refresh_editor_client_id: String::new(),
            oauth_refresh_editor_client_secret: SecretString::default(),
            oauth_refresh_editor_refresh_token: SecretString::default(),
            active_view: WorkspaceView::Overview,
            pending_evidence: None,
            pending_verification_failure: None,
            pending_mismatches: Vec::new(),
            pending_batch_evidence: HashMap::new(),
            pending_batch_mismatches: HashMap::new(),
            pending_checkpoint: None,
            pending_batch_checkpoints: HashMap::new(),
            pending_db_events: Vec::new(),
            deferred_events: VecDeque::new(),
            run_started_at: None,
            // Migration windows are log-heavy and commonly run in dark,
            // low-glare operator environments.
            dark_mode: appearance.dark_mode,
            theme: appearance.theme,
            ui_scale: appearance.ui_scale,
            language: appearance.language,
            branding: crate::branding::OperatorBranding::load(),
            bulk_live_confirm_open: false,
            bulk_live_confirm_focus_requested: false,
            bulk_live_confirmed: false,
            bulk_destination_loss_acknowledged: false,
            bulk_destination_case_acknowledged: false,
            live_destination_loss_acknowledged: false,
            bulk_confirmation_summary: None,
            bulk_confirmation_identity: None,
            bulk_mode: BatchExecutionMode::Preflight,
            bulk_clear_confirm_open: false,
            pending_bulk_import: None,
            pending_plaintext_import: None,
            bulk_plaintext_import_acknowledged: false,
            plaintext_import_dialog_acknowledged: false,
            pending_sheet_import: None,
            bulk_sheet_index: 0,
            bulk_import_receiver: None,
            bulk_live_run: false,
            bulk_retry_scope: BulkRetryScope::default(),
            verification_exception_operator: String::new(),
            verification_exception_reason: String::new(),
            verification_search: String::new(),
            verification_filter: "all".into(),
            verification_attention_reason: None,
            verification_visible_indices: Vec::new(),
            verification_filter_cache_search: String::new(),
            verification_filter_cache_state: String::new(),
            verification_filter_cache_reason: None,
            verification_filter_cache_offset: u32::MAX,
            verification_filter_cache_revision: None,
            verification_filter_cache_project: None,
            verification_filter_cache_rows: usize::MAX,
            activity_show_all: false,
            activity_search: String::new(),
            activity_status_filter: "all".into(),
            reopen_reason: String::new(),
            ui_snapshot: WorkspaceSnapshot::default(),
            snapshot_worker: None,
            run_telemetry: Default::default(),
            activity_window_spec: String::new(),
            historical_mailbox_offset: 0,
            historical_mailbox_cursor: None,
            historical_mailbox_cursor_stack: Vec::new(),
            verification_offset: 0,
            verification_cursor: None,
            verification_cursor_stack: Vec::new(),
            mismatch_inspector: Default::default(),
            observed_folder_names: Default::default(),
            source_provider: ProviderPreset::GenericImap,
            destination_provider: ProviderPreset::GenericImap,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_store_failure_is_a_structured_bootstrap_failure() {
        let failure = temporary_state_store("disk I/O error".into(), || {
            Err(rusqlite::Error::InvalidQuery)
        })
        .err()
        .expect("a failing in-memory store must not produce a session");
        assert_eq!(
            failure.durable,
            "Persistent SQLite state unavailable: disk I/O error"
        );
        assert!(
            failure
                .temporary
                .starts_with("SQLite in-memory store unavailable: ")
        );
        let rendered = failure.to_string();
        assert!(
            rendered.starts_with(BootstrapFailure::SUMMARY),
            "{rendered}"
        );
        assert!(rendered.contains("disk I/O error"), "{rendered}");
        assert!(rendered.contains(&failure.temporary), "{rendered}");
    }

    #[test]
    fn temporary_store_success_keeps_the_non_durable_warning() {
        let (_, warning) =
            temporary_state_store("locked".into(), core::StateStore::in_memory).unwrap();
        assert_eq!(
            warning.as_deref(),
            Some("Persistent SQLite state unavailable: locked")
        );
    }

    #[test]
    fn bootstrap_summary_matches_the_localized_english_copy() {
        assert_eq!(
            crate::ui::UiLanguage::English.text("ui.bootstrap-failure-summary"),
            BootstrapFailure::SUMMARY
        );
    }
}
