use crate::{
    Event, core,
    credentials::{CleanupGuard, SecretString},
    runner::{
        request_engine_version_probe, run_dovecot_destination_preflight, run_dovecot_verification,
        run_streaming,
    },
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
}

pub(crate) fn spawn_single_run_worker(spec: SingleRunWorkerSpec) {
    thread::spawn(move || {
        let SingleRunWorkerSpec {
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
        } = spec;
        let _cleanup_guard = CleanupGuard::new(cleanup);
        // Version metadata is best effort and must never delay admission or
        // block the UI. Probe it alongside the actual worker instead of on
        // the controller/render thread; the durable event is applied if the
        // probe finishes while this run is still active.
        request_engine_version_probe(&executable, &tx, &run_id, &job_id);
        let worker_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut result = run_streaming(
                &executable,
                &args,
                &env,
                &tx,
                &run_id,
                &job_id,
                "",
                &cancel,
                &output_secrets,
                timeout,
                engine == core::Engine::Dovecot && !dry_run,
            );
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
                    .map(|evidence| {
                        let _ = tx.send(Event::Evidence(evidence));
                        stream
                    })
                    .map_err(|error| {
                        let _ = tx.send(Event::VerificationFailed(error.clone()));
                        format!("migration completed; Dovecot verification failed: {error}")
                    })
                });
            }
            if let Ok(stream) = &result
                && let Some(evidence) = stream.imapsync_evidence.clone()
            {
                let _ = tx.send(Event::Evidence(evidence));
            }
            let _ = tx.send(Event::Finished(result.map(|stream| stream.outcome)));
        }));
        if worker_result.is_err() {
            let _ = tx.send(Event::Finished(Err(
                "single-run worker panicked; migration requires operator review".into(),
            )));
        }
    });
}
