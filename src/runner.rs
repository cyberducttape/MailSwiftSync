use std::{
    collections::{HashMap, HashSet},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::{
    BoundedLineBuffer, Event, MAX_DIAGNOSTIC_LINE_BYTES, MAX_PROCESS_TAIL_BYTES,
    MAX_PROCESS_TAIL_LINES, OutputObserver, PROCESS_REGISTRATION_ACK_TIMEOUT, StreamOutcome, core,
    credentials::SecretString,
    process::{
        ProcessOutcome, attach_child_supervisor, collect_redacted_lines_with_callback,
        configure_process_group, for_each_lossy_line, process_identity, terminate_process_group,
        wait_with_timeout,
    },
    truncate_utf8, verification,
};

#[derive(Debug)]
pub(crate) struct StreamResult {
    pub(crate) outcome: StreamOutcome,
    pub(crate) imapsync_evidence: Option<core::MailboxEvidence>,
}

// The process runner keeps each security-sensitive input explicit at the call
// site: executable, args, environment, event sink, redaction prefix/secrets,
// cancellation, and operator-selected timeout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_streaming(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
    prefix: &str,
    cancel: &AtomicBool,
    secrets: &[SecretString],
    timeout: Duration,
    dovecot_exit_two_is_delta: bool,
) -> Result<StreamResult, String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let _child_supervisor = match attach_child_supervisor(&child) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "could not establish descendant process supervision for {executable}: {error}"
            ));
        }
    };
    let identity = process_identity(child.id());
    let (start_ticks, process_group, session_id) = identity
        .map(|(start, group, session)| (Some(start), Some(group), Some(session)))
        .unwrap_or((None, None, None));
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stdout pipe unavailable; child cancelled before supervision".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stderr pipe unavailable; child cancelled before supervision".into());
        }
    };
    let out_tx = tx.clone();
    let out_prefix = prefix.to_owned();
    let out_secrets = secrets.to_vec();
    let tail = Arc::new(Mutex::new(BoundedLineBuffer::new()));
    let evidence = Arc::new(Mutex::new(
        verification::ImapsyncEvidenceAccumulator::default(),
    ));
    let dropped_diagnostics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let out_tail = Arc::clone(&tail);
    let out_evidence = Arc::clone(&evidence);
    let dovecot_checkpoint = Arc::new(Mutex::new(None::<String>));
    let out_dovecot_checkpoint = Arc::clone(&dovecot_checkpoint);
    let out_dropped_diagnostics = Arc::clone(&dropped_diagnostics);
    let out_run_id = run_id.to_owned();
    let out_job_id = job_id.to_owned();
    let out_thread = thread::spawn(move || {
        for_each_lossy_line(stdout, |line| {
            let mut safe = line;
            for secret in &out_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret.as_str(), "[REDACTED]");
                }
            }
            record_process_tail(&out_tail, &safe);
            if let Ok(mut evidence) = out_evidence.lock() {
                evidence.observe(&safe);
            }
            if dovecot_exit_two_is_delta
                && let Some(candidate) = dovecot_state_candidate(&safe)
                && let Ok(mut checkpoint) = out_dovecot_checkpoint.lock()
            {
                *checkpoint = Some(candidate);
            }
            if out_tx
                .try_send(Event::RunLine {
                    run_id: out_run_id.clone(),
                    job_id: out_job_id.clone(),
                    text: format!("{out_prefix}{safe}"),
                })
                .is_err()
            {
                out_dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    let err_tx = tx.clone();
    let err_prefix = prefix.to_owned();
    let err_secrets = secrets.to_vec();
    let err_tail = Arc::clone(&tail);
    let err_evidence = Arc::clone(&evidence);
    let err_dropped_diagnostics = Arc::clone(&dropped_diagnostics);
    let err_run_id = run_id.to_owned();
    let err_job_id = job_id.to_owned();
    let err_thread = thread::spawn(move || {
        for_each_lossy_line(stderr, |line| {
            let mut safe = line;
            for secret in &err_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret.as_str(), "[REDACTED]");
                }
            }
            record_process_tail(&err_tail, &safe);
            if let Ok(mut evidence) = err_evidence.lock() {
                evidence.observe(&safe);
            }
            if err_tx
                .try_send(Event::RunLine {
                    run_id: err_run_id.clone(),
                    job_id: err_job_id.clone(),
                    text: format!("{err_prefix}[stderr] {safe}"),
                })
                .is_err()
            {
                err_dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    // Start both drainers before the reliable lifecycle send. If the
    // bounded event queue is temporarily full, this send may wait, but the
    // child pipes are already being drained and cannot deadlock the engine.
    let (registration_tx, registration_rx) = mpsc::sync_channel(1);
    let registration_deadline = std::time::Instant::now() + PROCESS_REGISTRATION_ACK_TIMEOUT;
    let mut process_started_event = Some(Event::ProcessStarted(
        run_id.to_owned(),
        job_id.to_owned(),
        child.id(),
        start_ticks,
        process_group,
        session_id,
        executable.to_owned(),
        registration_tx,
    ));
    let process_started = loop {
        if cancel.load(Ordering::Relaxed) {
            break false;
        }
        let remaining = registration_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break false;
        }
        let Some(event) = process_started_event.take() else {
            break false;
        };
        match tx.try_send(event) {
            Ok(()) => break true,
            Err(mpsc::TrySendError::Full(event)) => {
                process_started_event = Some(event);
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break false,
        }
    };
    let result = if process_started {
        let registration = loop {
            if cancel.load(Ordering::Relaxed) {
                break Err("cancelled before durable process registration".to_owned());
            }
            let remaining =
                registration_deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break Err("durable process registration acknowledgement timed out".to_owned());
            }
            match registration_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err("durable process registration response was lost".to_owned());
                }
            }
        };
        if let Err(error) = registration {
            cancel.store(true, Ordering::Relaxed);
            let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
            Err(format!(
                "process registration failed; child cancelled: {error}"
            ))
        } else {
            match wait_with_timeout(&mut child, timeout, cancel) {
                Err(error) => Err(error.to_string()),
                Ok(ProcessOutcome {
                    cancelled: true, ..
                }) => Err("cancelled by operator".into()),
                Ok(ProcessOutcome {
                    timed_out: true, ..
                }) => Err("migration exceeded its configured execution timeout".into()),
                Ok(ProcessOutcome {
                    exit_code: Some(0), ..
                }) => Ok(StreamOutcome::Completed),
                Ok(ProcessOutcome {
                    exit_code: Some(2), ..
                }) if dovecot_exit_two_is_delta => Ok(StreamOutcome::DeltaRequired),
                Ok(ProcessOutcome { exit_code, .. }) => {
                    Err(format!("process exited with code {:?}", exit_code))
                }
            }
        }
    } else {
        // ProcessStarted is the reliable hand-off to the durable controller.
        // Continuing after a disconnected event channel would leave a live
        // child with no possible process registration or cancellation owner.
        cancel.store(true, Ordering::Relaxed);
        let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
        Err("process-start event channel disconnected; child cancelled before supervision".into())
    };
    let stdout_reader = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned());
    let stderr_reader = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned());
    let reader_error = stdout_reader
        .as_ref()
        .err()
        .cloned()
        .or_else(|| stderr_reader.as_ref().err().cloned())
        .or_else(|| {
            stdout_reader
                .as_ref()
                .ok()
                .and_then(|result| result.as_ref().err())
                .map(|error| format!("stdout reader failed: {error}"))
        })
        .or_else(|| {
            stderr_reader
                .as_ref()
                .ok()
                .and_then(|result| result.as_ref().err())
                .map(|error| format!("stderr reader failed: {error}"))
        });
    let dropped = dropped_diagnostics.load(Ordering::Relaxed);
    if dropped > 0 {
        // This is the durable accounting marker for lossy UI streaming. The
        // reader threads have already joined, so waiting here cannot block a
        // child pipe; it ensures a saturated event queue cannot silently
        // erase the fact that diagnostics were omitted.
        let _ = tx.send(Event::RunLine {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
            text: format!(
                "{prefix}[diagnostics] {dropped} output line(s) omitted because the operator event queue was full"
            ),
        });
    }
    // The process identity is only valid for this attempt. Clear it before
    // returning so a transient retry (or a crash during its backoff) cannot
    // leave an exited PID looking like an active orphan.
    let process_end_error = tx
        .send(Event::ProcessEnded {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
        })
        .err()
        .map(|_| "process event channel disconnected while clearing process identity".to_owned());
    match result {
        Ok(outcome) if reader_error.is_none() => {
            if let Some(error) = process_end_error {
                return Err(error);
            }
            let imapsync_evidence = evidence
                .lock()
                .map_err(|_| "imapsync evidence collector was poisoned".to_owned())?;
            let imapsync_evidence = imapsync_evidence.evidence();
            let checkpoint = dovecot_checkpoint
                .lock()
                .map_err(|_| "Dovecot checkpoint collector was poisoned".to_owned())?
                .clone();
            if let Some(value) = &checkpoint {
                tx.send(Event::Checkpoint {
                    run_id: run_id.to_owned(),
                    job_id: job_id.to_owned(),
                    value: value.clone(),
                })
                .map_err(|_| {
                    "process event channel disconnected while delivering checkpoint".to_owned()
                })?;
            }
            Ok(StreamResult {
                outcome,
                imapsync_evidence,
            })
        }
        result => {
            let reader_error = reader_error.unwrap_or_default();
            let process_error = match result {
                Ok(_) => String::new(),
                Err(error) => error,
            };
            let error = [
                process_error,
                reader_error,
                process_end_error.unwrap_or_default(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
            let recent = process_tail_text(&tail);
            if recent.is_empty() {
                Err(error)
            } else {
                Err(format!("{error}; recent output: {recent}"))
            }
        }
    }
}

/// Dovecot emits the stateful-sync resume value as a compact, single-line
/// standard-base64 token. Both padded and unpadded forms are accepted; the
/// durable store applies the same validation before persistence.
pub(crate) fn dovecot_state_candidate(line: &str) -> Option<String> {
    if core::valid_dovecot_checkpoint(line) {
        Some(line.to_owned())
    } else {
        None
    }
}

pub(crate) fn record_process_tail(tail: &Mutex<BoundedLineBuffer>, line: &str) {
    if let Ok(mut tail) = tail.lock() {
        let line = truncate_utf8(line, MAX_DIAGNOSTIC_LINE_BYTES);
        tail.push_bounded(line, MAX_PROCESS_TAIL_LINES, MAX_PROCESS_TAIL_BYTES);
    }
}

fn process_tail_text(tail: &Mutex<BoundedLineBuffer>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

pub(crate) fn run_capture_lines(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    cancel: &AtomicBool,
    secrets: &[SecretString],
    timeout: Duration,
    observer: Option<OutputObserver>,
) -> Result<(ProcessOutcome, Vec<String>, bool), String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let _child_supervisor = match attach_child_supervisor(&child) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "could not establish descendant process supervision for {executable}: {error}"
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stdout pipe unavailable; child cancelled before supervision".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stderr pipe unavailable; child cancelled before supervision".into());
        }
    };
    let out_secrets = secrets.to_vec();
    let out_observer = observer.clone();
    let out_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stdout, &out_secrets, |line| {
            if let Some(observer) = &out_observer {
                observer(line);
            }
        })
    });
    let err_secrets = secrets.to_vec();
    let err_observer = observer;
    let err_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stderr, &err_secrets, |line| {
            if let Some(observer) = &err_observer {
                observer(line);
            }
        })
    });
    let status = wait_with_timeout(&mut child, timeout, cancel).map_err(|error| error.to_string());
    let stdout_lines = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned())?
        .map_err(|error| format!("stdout reader failed: {error}"));
    let stderr_lines = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned())?
        .map_err(|error| format!("stderr reader failed: {error}"));
    let stdout_output = stdout_lines?;
    let stderr_output = stderr_lines?;
    let capture_truncated = stdout_output.truncated || stderr_output.truncated;
    let mut lines = stdout_output.lines;
    lines.extend(
        stderr_output
            .lines
            .into_iter()
            .map(|line| format!("[stderr] {line}")),
    );
    let status = status?;
    if capture_truncated {
        lines.push(
            "[diagnostics truncated; semantic verification continued from the full stream]".into(),
        );
    }
    Ok((status, lines, capture_truncated))
}

/// Ask an engine for its version without passing credentials or mailbox
/// arguments. This is best-effort metadata: an old wrapper may not implement
/// `--version`, in which case the run explicitly remains unversioned.
pub(crate) fn probe_engine_version(executable: &str) -> Option<String> {
    let cancel = AtomicBool::new(false);
    let mut candidates = vec![(executable.to_owned(), vec!["--version".into()])];
    if std::path::Path::new(executable)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("doveadm"))
    {
        let sibling = std::path::Path::new(executable)
            .parent()
            .map(|parent| parent.join("dovecot"))
            .unwrap_or_else(|| std::path::PathBuf::from("dovecot"));
        candidates.push((
            sibling.to_string_lossy().into_owned(),
            vec!["--version".into()],
        ));
    }
    for (candidate, args) in candidates {
        let Ok((status, lines, _)) = run_capture_lines(
            &candidate,
            &args,
            &[],
            &cancel,
            &[],
            Duration::from_secs(5),
            None,
        ) else {
            continue;
        };
        if status.exit_code == Some(0)
            && let Some(line) = lines
                .into_iter()
                .map(|line| line.trim().to_owned())
                .find(|line| !line.is_empty() && line.len() <= 512)
        {
            return Some(line);
        }
    }
    None
}

#[derive(Default)]
struct EngineVersionProbeCache {
    completed: HashMap<String, Option<String>>,
    in_flight: HashSet<String>,
}

fn engine_version_probe_cache() -> &'static Mutex<EngineVersionProbeCache> {
    static CACHE: OnceLock<Mutex<EngineVersionProbeCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(EngineVersionProbeCache::default()))
}

/// Schedule best-effort version metadata without blocking the controller.
/// The key includes executable contents so replacing a binary at the same
/// path gets a fresh probe. Completed and in-flight entries are shared across
/// workers, which keeps a large batch from spawning one metadata process per
/// mailbox.
pub(crate) fn request_engine_version_probe(
    executable: &str,
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
) {
    let key = format!(
        "{}\0{}",
        executable,
        crate::plan_identity::executable_content_identity(executable)
    );
    let cached = {
        let Ok(mut cache) = engine_version_probe_cache().lock() else {
            return;
        };
        if let Some(version) = cache.completed.get(&key) {
            Some(version.clone())
        } else if cache.in_flight.insert(key.clone()) {
            None
        } else {
            return;
        }
    };
    if let Some(Some(version)) = cached {
        let _ = tx.try_send(crate::Event::EngineVersion {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
            version,
        });
        return;
    }
    if cached.is_some() {
        return;
    }

    let executable = executable.to_owned();
    let run_id = run_id.to_owned();
    let job_id = job_id.to_owned();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let version = probe_engine_version(&executable);
        if let Ok(mut cache) = engine_version_probe_cache().lock() {
            cache.in_flight.remove(&key);
            cache.completed.insert(key, version.clone());
        }
        if let Some(version) = version {
            let _ = tx.try_send(crate::Event::EngineVersion {
                run_id,
                job_id,
                version,
            });
        }
    });
}

pub(crate) fn run_dovecot_destination_preflight(
    commands: &[(String, Vec<String>)],
    tx: &mpsc::SyncSender<crate::Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
    run_id: &str,
    job_id: &str,
) -> Result<(), String> {
    for (index, (executable, args)) in commands.iter().enumerate() {
        let (status, lines, _) =
            run_capture_lines(executable, args, &[], cancel, &[], timeout, None)?;
        for line in lines {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!("{prefix}[destination preflight/{}] {line}", index + 1),
            });
        }
        if status.exit_code != Some(0) {
            return Err(format!(
                "Dovecot destination preflight command {} exited with code {:?}",
                index + 1,
                status.exit_code
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_dovecot_verification(
    commands: &[(String, Vec<String>)],
    verification_env: &[(String, SecretString)],
    secrets: &[SecretString],
    tx: &mpsc::SyncSender<crate::Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
    run_id: &str,
    job_id: &str,
) -> Result<core::MailboxEvidence, String> {
    let mut reports = Vec::with_capacity(commands.len());
    for (index, (verify_exe, verify_args)) in commands.iter().enumerate() {
        let accumulator = Arc::new(Mutex::new(verification::DovecotStatusAccumulator::default()));
        let observer_accumulator = Arc::clone(&accumulator);
        let observer: OutputObserver = Arc::new(move |line| {
            if let Ok(mut accumulator) = observer_accumulator.lock() {
                accumulator.observe(line);
            }
        });
        let (status, report, truncated) = run_capture_lines(
            verify_exe,
            verify_args,
            if index == 0 { verification_env } else { &[] },
            cancel,
            secrets,
            timeout,
            Some(observer),
        )?;
        for line in &report {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!("{prefix}[verification/{}] {line}", index + 1),
            });
        }
        if status.exit_code != Some(0) {
            return Err(format!(
                "Dovecot verification command {} exited with code {:?}",
                index + 1,
                status.exit_code
            ));
        }
        if truncated {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!(
                    "{prefix}[verification/{}] diagnostic transcript truncated; semantic accumulator processed the complete stream",
                    index + 1
                ),
            });
        }
        let status = accumulator
            .lock()
            .map_err(|_| "Dovecot verification accumulator was poisoned".to_owned())?
            .clone();
        reports.push((report, status));
    }
    if reports.len() < 2 {
        return Err("Dovecot verification returned incomplete reports".into());
    }
    verification::dovecot_evidence_from_accumulators(&reports[0].1, &reports[1].1)
        .ok_or_else(|| "Dovecot status output was incomplete".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::request_engine_version_probe;
    use crate::Event;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::mpsc,
        time::{Duration, Instant},
    };

    #[test]
    fn engine_version_probe_is_asynchronous() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-version-probe-{}-{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, "#!/bin/sh\nsleep 1\nprintf 'test-engine 1.0\\n'\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let (tx, rx) = mpsc::sync_channel(2);

        let started = Instant::now();
        request_engine_version_probe(&path.to_string_lossy(), &tx, "run", "job");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "version probe blocked the caller"
        );

        let event = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("asynchronous version result");
        assert!(matches!(
            event,
            Event::EngineVersion { version, .. } if version == "test-engine 1.0"
        ));
        fs::remove_file(path).unwrap();
    }
}
