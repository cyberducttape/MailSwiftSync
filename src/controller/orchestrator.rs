use crate::{
    Event, core,
    credentials::{CleanupGuard, SecretString},
    runner::{
        RunContext, TerminalEvidenceSource, message_verification_enabled,
        persist_engine_identity_before_launch, probe_engine_version, resolve_imapsync_identity,
        run_dovecot_destination_preflight, run_dovecot_verification, run_imap_message_verification,
        run_streaming, send_reliable_event, terminal_evidence_source,
    },
    verification::ImapsyncOutputProfile,
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc},
    thread,
    time::Duration,
};

/// Everything a single execution worker needs after the controller has
/// completed admission. Keeping this owned specification outside egui makes
/// the same process lifecycle reusable by GUI and headless callers.
pub(crate) struct SingleRunWorkerSpec {
    pub(crate) form: crate::Form,
    pub(crate) executable: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, SecretString)>,
    pub(crate) cleanup: Vec<PathBuf>,
    pub(crate) tx: mpsc::SyncSender<Event>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) run_id: String,
    pub(crate) job_id: String,
    pub(crate) engine: core::Engine,
    pub(crate) dry_run: bool,
    pub(crate) verification: Vec<(String, Vec<String>)>,
    pub(crate) destination_preflight: Vec<(String, Vec<String>)>,
    pub(crate) verification_env: Vec<(String, SecretString)>,
    pub(crate) verification_secret: SecretString,
    pub(crate) output_secrets: Vec<SecretString>,
    pub(crate) timeout: Duration,
    pub(crate) project_id: String,
    pub(crate) diagnostic_logger: Option<Arc<crate::DiagnosticLogger>>,
}

pub(crate) fn spawn_single_run_worker(spec: SingleRunWorkerSpec) {
    thread::spawn(move || {
        let SingleRunWorkerSpec {
            form,
            executable,
            args,
            env,
            cleanup,
            tx,
            cancel,
            run_id,
            job_id,
            engine,
            dry_run,
            verification,
            destination_preflight,
            verification_env,
            verification_secret,
            output_secrets,
            timeout,
            project_id,
            diagnostic_logger,
        } = spec;
        let _cleanup_guard = CleanupGuard::new(cleanup);
        // Resolve the executable before launch. Imapsync's result selects the
        // only parser grammar permitted to create verification evidence.
        let (engine_version, mut imapsync_output_profile) = if engine == core::Engine::ImapSync {
            let identity = resolve_imapsync_identity(&executable);
            (identity.version, identity.output_profile)
        } else {
            (
                probe_engine_version(&executable).unwrap_or_else(|| "unknown".into()),
                ImapsyncOutputProfile::Unknown,
            )
        };
        let identity_persisted = if let Err(error) =
            persist_engine_identity_before_launch(&tx, &run_id, &job_id, &engine_version)
        {
            imapsync_output_profile = ImapsyncOutputProfile::Unknown;
            let _ = tx.send(Event::RunLine {
                run_id: run_id.clone(),
                job_id: job_id.clone(),
                text: format!(
                    "[verification] engine identity is not durable; output evidence disabled: {error}"
                ),
            });
            false
        } else {
            true
        };
        if engine == core::Engine::ImapSync
            && identity_persisted
            && imapsync_output_profile == ImapsyncOutputProfile::Unknown
        {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.clone(),
                job_id: job_id.clone(),
                text: crate::verification::unqualified_imapsync_message(&engine_version),
            });
        }
        let worker_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut result = run_streaming(RunContext {
                executable: &executable,
                args: &args,
                env: &env,
                tx: &tx,
                run_id: &run_id,
                job_id: &job_id,
                project_id: &project_id,
                prefix: "",
                cancel: &cancel,
                secrets: &output_secrets,
                timeout,
                dovecot_exit_two_is_delta: engine == core::Engine::Dovecot && !dry_run,
                imapsync_output_profile,
                diagnostic_logger: diagnostic_logger.clone(),
            });
            if result.is_ok() && !destination_preflight.is_empty() {
                result = result.and_then(|outcome| {
                    run_dovecot_destination_preflight(
                        &destination_preflight,
                        &tx,
                        &cancel,
                        timeout,
                        "",
                        &run_id,
                        &job_id,
                    )
                    .map(|_| outcome)
                });
            }
            if result.is_ok() && !verification.is_empty() {
                result = result.and_then(|stream| {
                    run_dovecot_verification(
                        &verification,
                        &verification_env,
                        std::slice::from_ref(&verification_secret),
                        &tx,
                        &cancel,
                        timeout,
                        "",
                        &run_id,
                        &job_id,
                    )
                    .and_then(|evidence| {
                        send_reliable_event(&tx, Event::Evidence(evidence)).map(|_| stream)
                    })
                    .map_err(|error| {
                        let detail =
                            format!("migration completed; Dovecot verification failed: {error}");
                        match send_reliable_event(&tx, Event::VerificationFailed(detail.clone())) {
                            Ok(()) => detail,
                            Err(delivery_error) => format!(
                                "{detail}; verification event delivery failed: {delivery_error}"
                            ),
                        }
                    })
                });
            }
            if result.is_ok()
                && !dry_run
                && engine == core::Engine::ImapSync
                && message_verification_enabled(&form)
            {
                result = result.and_then(|stream| {
                    match run_imap_message_verification(&form, &job_id, &run_id, &cancel) {
                        Ok((evidence, mismatches)) => {
                            send_reliable_event(
                                &tx,
                                Event::MessageMismatches {
                                    run_id: run_id.clone(),
                                    job_id: job_id.clone(),
                                    mismatches,
                                },
                            )?;
                            send_reliable_event(&tx, Event::Evidence(evidence))?;
                        }
                        Err(error) => {
                            send_reliable_event(
                                &tx,
                                Event::VerificationFailed(format!(
                                    "message-level IMAP verification unavailable: {error}"
                                )),
                            )?;
                            // The transfer already succeeded. Missing
                            // post-transfer evidence is durable operator
                            // review, not a failed migration.
                        }
                    }
                    Ok(stream)
                });
            }
            if terminal_evidence_source(
                &form,
                false,
                result
                    .as_ref()
                    .ok()
                    .and_then(|stream| stream.imapsync_evidence.as_ref())
                    .is_some(),
            ) == TerminalEvidenceSource::Engine
                && let Ok(stream) = &result
                && let Some(evidence) = stream.imapsync_evidence.clone()
                && let Err(error) = send_reliable_event(&tx, Event::Evidence(evidence))
            {
                result = Err(format!("terminal evidence delivery failed: {error}"));
            }
            if let Err(error) =
                send_reliable_event(&tx, Event::Finished(result.map(|stream| stream.outcome)))
            {
                eprintln!("reliable terminal event delivery failed: {error}");
            }
        }));
        if worker_result.is_err()
            && let Err(error) = send_reliable_event(
                &tx,
                Event::Finished(Err(
                    "single-run worker panicked; migration requires operator review".into(),
                )),
            )
        {
            eprintln!("reliable panic-recovery event delivery failed: {error}");
        }
    });
}
