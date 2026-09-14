use crate::*;

impl App {
    /// Construct the application against an explicit ledger path. Headless
    /// callers use this to avoid process-global environment mutation while
    /// retaining the same recovery and durable-state behavior as the GUI.
    pub(crate) fn from_state_path(state_override: Option<&std::path::Path>) -> Self {
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
                    std::fs::create_dir_all(parent)
                        .and_then(|_| restrict_directory_permissions(parent))
                        .err()
                        .map(|error| {
                            format!("Could not secure persistent state directory: {error}")
                        })
                });
        let instance_lock = state_path
            .as_ref()
            .map(|path| acquire_instance_lock(path.as_path()));
        let (store, mut persistence_warning) = match instance_lock.as_ref() {
            Some(Ok(_)) => match core::StateStore::open(
                state_path
                    .as_ref()
                    .expect("instance lock cannot exist without a state path"),
            ) {
                Ok(store) => (store, None),
                Err(error) => (
                    core::StateStore::in_memory().expect("SQLite memory store must be available"),
                    Some(format!("Persistent SQLite state unavailable: {error}")),
                ),
            },
            Some(Err(error)) => (
                core::StateStore::in_memory().expect("SQLite memory store must be available"),
                Some(format!("Persistent SQLite state unavailable: {error}")),
            ),
            None => (
                core::StateStore::in_memory().expect("SQLite memory store must be available"),
                Some(format!(
                    "Persistent SQLite state unavailable: {}",
                    path_error
                        .as_deref()
                        .unwrap_or("no durable state path is available")
                )),
            ),
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
        // Only the lock owner may reconcile stale runtime secrets. If any
        // recorded child identity could not be verified, fail closed and
        // preserve age-only secret directories: an unverified process may
        // still depend on its passfile. The operator can review and clean it
        // up after confirming the process is gone.
        if persistence_warning.is_none() && unverified_processes.is_empty() {
            cleanup_stale_secret_directories(&secret_runtime_base());
        }
        let mut initial_output_lines = persistence_warning.clone().map_or_else(
            || vec!["Ready. Start with Preflight against a test destination mailbox.".into()],
            |warning| {
                vec![
                    warning.clone(),
                    "WARNING: this session is not durable.".into(),
                ]
            },
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
                "Stale secret cleanup was deferred because an unverified process may still need its passfile."
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
            |project| match store.mailboxes(&project.id) {
                Ok(jobs) => !jobs.is_empty() && jobs.iter().all(|job| job.config.is_some()),
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
        let mut restored_bulk_jobs = Vec::new();
        let mut restored_bulk_job_ids = Vec::new();
        let restored_bulk_project_id = restored_project.as_ref().and_then(|project| {
            if project.name.trim().is_empty() || !restored_project_is_batch {
                return None;
            }
            let jobs = match store.mailboxes(&project.id) {
                Ok(jobs) => jobs,
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent batch restore failed; execution is blocked: {error}"
                    ));
                    return None;
                }
            };
            for job in jobs {
                let profile = match decode_persisted_batch_profile(job.config.as_deref(), &job.id) {
                    Ok(profile) => profile,
                    Err(error) => {
                        persistence_warning = Some(format!("{error}; execution is blocked"));
                        return None;
                    }
                };
                let mut profile = profile;
                if profile.destination_tls.is_empty() {
                    profile.destination_tls = default_destination_tls();
                }
                restored_bulk_jobs.push(BulkJob {
                    label: format!("{} → {}", job.source_mailbox, job.destination_mailbox),
                    form: Form {
                        profile,
                        source_password: SecretString::default(),
                        destination_password: SecretString::default(),
                        dry_run: true,
                    },
                    state: display_job_state(&job.state).into(),
                });
                restored_bulk_job_ids.push(job.id);
            }
            Some(project.id.clone())
        });
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
        let restored_bulk_preflight_credential_fingerprints = vec![None; restored_bulk_jobs.len()];
        let restored_bulk_job_index_by_id = restored_bulk_job_ids
            .iter()
            .enumerate()
            .map(|(index, job_id)| (job_id.clone(), index))
            .collect();
        Self {
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
            bulk_jobs: restored_bulk_jobs,
            bulk_open: false,
            settings_open: false,
            bulk_search: String::new(),
            bulk_state_filter: "all".into(),
            bulk_selected_ids: HashSet::new(),
            bulk_visible_indices: Vec::new(),
            bulk_search_values: Vec::new(),
            bulk_filter_cache_search: String::new(),
            bulk_filter_cache_state: String::new(),
            bulk_filter_cache_generation: u64::MAX,
            bulk_jobs_generation: 0,
            bulk_message: if restored_bulk_project_id.is_some() {
                "Restored durable batch queue; credentials must be entered again before validation."
                    .into()
            } else {
                "Import a CSV, XLS, or XLSX file to build a reviewable queue.".into()
            },
            advanced_open: false,
            // Do not interrupt first launch with a configuration dialog. The
            // conservative Auto engine is already selected and can be
            // changed from the migration plan when the endpoints are known.
            engine_open: false,
            store,
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
            locked_profile: None,
            cancel_requested: None,
            bulk_project_id: restored_bulk_project_id,
            bulk_job_ids: restored_bulk_job_ids,
            bulk_job_index_by_id: restored_bulk_job_index_by_id,
            bulk_preflight_credential_fingerprints: restored_bulk_preflight_credential_fingerprints,
            preflight: Vec::new(),
            capability_receiver: None,
            capability_probe_request_id: None,
            capability_probe_fingerprint: None,
            capability_observation_fingerprint: None,
            live_auth_receiver: None,
            start_credentials_receiver: None,
            start_credentials_plan: None,
            credentials_ready_for_start: false,
            live_auth_proof: None,
            source_capabilities: None,
            destination_capabilities: None,
            live_confirm_open: false,
            live_confirmed: false,
            live_confirmation_plan: None,
            durability_error: false,
            durability_recovery_pending: false,
            stop_confirm_open: false,
            keyring_open: false,
            active_view: WorkspaceView::Overview,
            pending_evidence: None,
            pending_batch_evidence: HashMap::new(),
            pending_checkpoint: None,
            pending_batch_checkpoints: HashMap::new(),
            pending_db_events: Vec::new(),
            deferred_events: VecDeque::new(),
            run_started_at: None,
            // Migration windows are log-heavy and commonly run in dark,
            // low-glare operator environments.
            dark_mode: appearance.dark_mode,
            ui_scale: appearance.ui_scale,
            bulk_live_confirm_open: false,
            bulk_live_confirmed: false,
            bulk_confirmation_summary: None,
            bulk_summary: None,
            bulk_mode: BatchExecutionMode::Preflight,
            bulk_clear_confirm_open: false,
            pending_bulk_import: None,
            pending_sheet_import: None,
            bulk_sheet_index: 0,
            bulk_import_receiver: None,
            bulk_live_run: false,
            bulk_retry_scope: BulkRetryScope::default(),
            bulk_source_keyring_apply: String::new(),
            bulk_destination_keyring_apply: String::new(),
            verification_exception_operator: String::new(),
            verification_exception_reason: String::new(),
            verification_search: String::new(),
            verification_filter: "all".into(),
            verification_visible_indices: Vec::new(),
            verification_filter_cache_search: String::new(),
            verification_filter_cache_state: String::new(),
            verification_filter_cache_offset: u32::MAX,
            verification_filter_cache_revision: None,
            activity_show_all: false,
            activity_search: String::new(),
            activity_status_filter: "all".into(),
            reopen_reason: String::new(),
            ui_snapshot: WorkspaceSnapshot::default(),
            historical_mailbox_offset: 0,
            verification_offset: 0,
            source_provider: ProviderPreset::GenericImap,
            destination_provider: ProviderPreset::GenericImap,
        }
    }
}
