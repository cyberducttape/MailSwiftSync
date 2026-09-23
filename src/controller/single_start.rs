use crate::*;

impl App {
    pub(crate) fn start(&mut self) {
        let live_requires_confirmation = !self.form.dry_run
            && (!self.live_confirmed
                || self.live_confirmation_plan.as_deref()
                    != Some(plan_fingerprint_digest(&self.form.plan_fingerprint()).as_str()));
        match single_start_decision(SingleStartContext {
            durable_view_stale: self.ui_snapshot.is_stale(),
            profile_available: self.profile_available,
            read_only_project: self.workspace_read_only,
            process_review_required: self.process_review_required,
            live_requires_confirmation,
        }) {
            SingleStartDecision::Block(block) => {
                self.set_status(block.message(), StatusSeverity::Error);
                return;
            }
            SingleStartDecision::ConfirmLive => {
                self.live_confirmed = false;
                self.live_confirmation_plan = None;
                self.live_confirm_open = true;
                return;
            }
            SingleStartDecision::Proceed => {}
        }
        if !self.credentials_ready_for_start && self.start_requires_keyring_load() {
            if self.start_credentials_receiver.is_none() {
                let mut form = self.form.clone();
                let plan = self.start_credentials_plan_marker();
                let (tx, rx) = mpsc::channel();
                self.start_credentials_receiver = Some(rx);
                self.start_credentials_plan = Some(plan);
                self.set_status(
                    "Loading migration credentials from the OS keyring…",
                    StatusSeverity::Info,
                );
                thread::spawn(move || {
                    let result = if form.dry_run {
                        form.load_configured_keyring_credentials()
                    } else {
                        form.reload_configured_keyring_credentials()
                    };
                    let _ = tx.send(result.map(|()| form));
                });
            }
            return;
        }
        self.credentials_ready_for_start = false;
        let credential_load: Result<(), String> = Ok(());
        if let Err(error) = credential_load {
            self.set_status(error, StatusSeverity::Error);
            return;
        }
        if let Err(e) = self.form.validate() {
            self.set_status(e, StatusSeverity::Error);
            return;
        }
        if let (Some(project_id), Some(job_id)) = (self.project_id.clone(), self.job_id.clone()) {
            let identity_matches = match (
                self.store.project(&project_id),
                self.store.mailbox_identity(&job_id),
            ) {
                (Ok(Some(project)), Ok(Some((source, destination, state)))) => {
                    let mailbox = core::MailboxJob {
                        id: job_id.clone(),
                        source_mailbox: source,
                        destination_mailbox: destination,
                        state,
                        config: None,
                    };
                    durable_single_identity_matches(&project, &mailbox, &self.form.profile)
                }
                (Err(error), _) | (_, Err(error)) => {
                    self.set_status(
                        format!("Could not read the durable mailbox identity; migration was not started: {error}"),
                        StatusSeverity::Error,
                    );
                    return;
                }
                (Ok(None), _) | (_, Ok(None)) => false,
            };
            if !identity_matches {
                if self.form.dry_run {
                    // A changed identity is a new durable plan. Keep the
                    // previous project history intact and create a fresh
                    // project/job below rather than attaching the run to the
                    // old mailbox record.
                    self.project_id = None;
                    self.job_id = None;
                    self.selected_project_id = None;
                } else {
                    self.set_status(
                        "The current mailbox identity differs from the durable project. Run a new dry preflight for this plan before starting live migration.",
                        StatusSeverity::Error,
                    );
                    return;
                }
            }
        }
        if !self.form.dry_run && self.form.requires_insecure_transport_ack() {
            self.set_status(
                "Live migration blocked: acknowledge the cleartext source-transport risk before continuing.",
                StatusSeverity::Warning,
            );
            return;
        }
        if self.requires_live_imaps_auth_probe() {
            let plan_fingerprint = plan_fingerprint_digest(&self.form.plan_fingerprint());
            let credential_fingerprint = self.form.credential_fingerprint();
            if !self
                .live_auth_proof
                .as_ref()
                .is_some_and(|proof| proof.matches(&plan_fingerprint, &credential_fingerprint))
            {
                self.start_live_imaps_auth_probe(plan_fingerprint, credential_fingerprint);
                return;
            }
        }
        self.durability_error = false;
        self.durability_recovery_pending = false;
        if !self.form.dry_run {
            if !self.persistence_available {
                self.set_status(
                    "Live migration is disabled because durable SQLite storage is unavailable.",
                    StatusSeverity::Error,
                );
                return;
            }
            let preflight_ready = match self.project_id.as_deref() {
                Some(project_id) => match self.store.project(project_id) {
                    Ok(Some(project)) => matches!(
                        project.phase,
                        core::Phase::Preflight
                            | core::Phase::Pilot
                            | core::Phase::Seed
                            | core::Phase::CatchUp
                            | core::Phase::FinalDelta
                            | core::Phase::Verification
                    ),
                    Ok(None) => false,
                    Err(error) => {
                        self.set_status(
                            format!("Could not read durable project readiness; migration was not started: {error}"),
                            StatusSeverity::Error,
                        );
                        return;
                    }
                },
                None => false,
            };
            let expected_plan = plan_fingerprint_digest(&self.form.plan_fingerprint());
            let plan_matches = match self.job_id.as_deref() {
                Some(job) => match self.store.preflight_plan(job) {
                    Ok(Some(plan)) => plan == expected_plan,
                    Ok(None) => false,
                    Err(error) => {
                        self.set_status(
                            format!("Could not read the durable preflight plan; migration was not started: {error}"),
                            StatusSeverity::Error,
                        );
                        return;
                    }
                },
                None => false,
            };
            let current_credential_binding = self.form.credential_binding_fingerprint();
            let credentials_match = self.preflight_credential_fingerprint.as_deref()
                == Some(current_credential_binding.as_str());
            let mailbox_ready = match self.job_id.as_deref() {
                Some(job) => match self.store.mailbox_state(job) {
                    Ok(Some(state)) => state == "ready" || state == "delta_required",
                    Ok(None) => false,
                    Err(error) => {
                        self.set_status(
                            format!("Could not read durable mailbox readiness; migration was not started: {error}"),
                            StatusSeverity::Error,
                        );
                        return;
                    }
                },
                None => false,
            };
            if !preflight_ready || !mailbox_ready || !plan_matches || !credentials_match {
                self.set_status(if credentials_match {
                    "Run a successful dry preflight for this exact mailbox plan before starting live migration."
                } else {
                    "Credentials changed or were reloaded since preflight. Run a new dry preflight before starting live migration."
                }, StatusSeverity::Warning);
                return;
            }
        }
        if self.project_id.is_none() {
            match self.store.create_project_with_mailbox(
                &self.form.profile.name,
                &self.form.profile.source_host,
                &self.form.profile.destination_host,
                &self.form.profile.source_user,
                &self.form.profile.destination_user,
            ) {
                Ok((project, job)) => {
                    self.project_id = Some(project.id.clone());
                    self.selected_project_id = Some(project.id.clone());
                    self.job_id = Some(job);
                }
                Err(error) => {
                    self.set_status(
                        format!("Could not create durable migration project: {error}"),
                        StatusSeverity::Error,
                    );
                    return;
                }
            }
        }
        let plan_fingerprint = plan_fingerprint_digest(&self.form.plan_fingerprint());
        let credential_fingerprint = self.form.credential_binding_fingerprint();
        let run_engine = self.form.engine();
        let run_dry_run = self.form.dry_run;
        let (run_project_id, run_job_id) = match (self.project_id.clone(), self.job_id.clone()) {
            (Some(project), Some(job)) => (project, job),
            _ => {
                self.set_status(
                    "Could not start without a durable mailbox project.",
                    StatusSeverity::Error,
                );
                return;
            }
        };
        let dovecot_checkpoint = if run_engine == core::Engine::Dovecot && !run_dry_run {
            match self.store.mailbox_checkpoint(&run_job_id) {
                Ok(checkpoint) => checkpoint,
                Err(error) => {
                    self.set_status(
                        format!("Could not read the durable Dovecot checkpoint; migration was not started: {error}"),
                        StatusSeverity::Error,
                    );
                    return;
                }
            }
        } else {
            None
        };
        let plan_snapshot = match self
            .form
            .plan_snapshot_with_checkpoint(dovecot_checkpoint.as_deref())
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.set_status(error, StatusSeverity::Error);
                return;
            }
        };
        let prepared = match self
            .form
            .prepared_command_with_throttle_divisor_and_checkpoint(1, dovecot_checkpoint.as_deref())
        {
            Ok(command) => command,
            Err(error) => {
                self.set_status(error, StatusSeverity::Error);
                return;
            }
        };
        let exe = prepared.executable;
        let args = prepared.args;
        let cleanup = prepared.cleanup;
        let prepared_env = prepared.env;
        let run_id = uuid::Uuid::new_v4().to_string();
        let worker_project_id = run_project_id.clone();
        let active_run = match admit_single_run(
            &self.store,
            SingleRunAdmission {
                project_id: run_project_id,
                job_id: run_job_id.clone(),
                run_id: run_id.clone(),
                engine: run_engine,
                dry_run: run_dry_run,
                plan_fingerprint,
                credential_fingerprint,
                plan_snapshot,
            },
        ) {
            Ok(context) => context,
            Err(error) => {
                cleanup_paths(&cleanup);
                self.set_status(error, StatusSeverity::Error);
                return;
            }
        };
        self.locked_profile = Some(self.form.profile.clone());
        self.live_auth_proof = None;
        self.live_confirmed = false;
        self.live_confirmation_plan = None;
        self.pending_checkpoint = None;
        self.run_id = Some(run_id.clone());
        self.active_run = Some(active_run);
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_requested = Some(cancel.clone());
        self.receiver = Some(rx);
        self.run_started_at = Some(std::time::Instant::now());
        self.set_status(
            if self.form.dry_run {
                "Preflight in progress"
            } else {
                "Sync in progress"
            },
            StatusSeverity::Info,
        );
        self.output = BoundedLineBuffer::from_one(format!(
            "Starting {} with {}…",
            if self.form.dry_run {
                "preflight"
            } else {
                "synchronization"
            },
            self.form.engine().label()
        ));
        let verification = if !self.form.dry_run && self.form.engine() == core::Engine::Dovecot {
            self.form.dovecot_verification_commands(false)
        } else {
            Vec::new()
        };
        let destination_preflight =
            if self.form.dry_run && self.form.engine() == core::Engine::Dovecot {
                self.form.dovecot_destination_preflight_commands()
            } else {
                Vec::new()
            };
        let verification_secret = self.form.source_password.clone();
        let verification_env = if self.form.local_doveadm() {
            vec![(
                "MAILSWIFTSYNC_IMAPC_PASSWORD".into(),
                self.form.source_password.clone(),
            )]
        } else {
            Vec::new()
        };
        let output_secrets = vec![
            self.form.source_password.clone(),
            self.form.destination_password.clone(),
        ];
        let migration_timeout =
            Duration::from_secs(self.form.profile.migration_timeout_hours * 60 * 60);
        spawn_single_run_worker(SingleRunWorkerSpec {
            executable: exe,
            args,
            env: prepared_env,
            cleanup,
            tx,
            cancel,
            run_id,
            job_id: run_job_id,
            project_id: worker_project_id,
            engine: run_engine,
            dry_run: run_dry_run,
            verification,
            destination_preflight,
            verification_env,
            verification_secret,
            output_secrets,
            timeout: migration_timeout,
            diagnostic_logger: self.diagnostic_logger.clone(),
        });
    }
}
