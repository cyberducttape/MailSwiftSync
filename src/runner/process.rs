//! Launching, registering, supervising, and capturing engine processes.

use super::*;

pub(crate) fn record_process_tail(tail: &Mutex<BoundedLineBuffer>, line: &str) {
    if let Ok(mut tail) = tail.lock() {
        let line = truncate_utf8(line, MAX_DIAGNOSTIC_LINE_BYTES);
        tail.push_bounded(line, MAX_PROCESS_TAIL_LINES, MAX_PROCESS_TAIL_BYTES);
    }
}

pub(super) fn process_tail_text(tail: &Mutex<BoundedLineBuffer>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

pub(super) fn register_process(
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
    executable: &str,
    child: &std::process::Child,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let (start_ticks, process_group, session_id) = process_identity(child.id())
        .map(|(start, group, session)| (Some(start), Some(group), Some(session)))
        .unwrap_or((None, None, None));
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        crate::Event::ProcessStarted(
            run_id.to_owned(),
            job_id.to_owned(),
            child.id(),
            start_ticks,
            process_group,
            session_id,
            executable.to_owned(),
            ack_tx,
        ),
    )?;
    let deadline = std::time::Instant::now() + PROCESS_REGISTRATION_ACK_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled before durable process registration".to_owned());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("durable process registration acknowledgement timed out".to_owned());
        }
        match ack_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => return Err(format!("process registration failed: {error}")),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("durable process registration response was lost".to_owned());
            }
        }
    }
}

/// Start MailSwiftSync's internal gatekeeper instead of the external engine.
/// The gatekeeper inherits the configured environment, waits for the durable
/// ProcessStarted acknowledgement, revalidates the transfer engine identity,
/// and only then launches the engine. This closes the crash window between OS
/// process creation and durable ownership without leaving a replacement window
/// during the durable acknowledgement wait.
#[cfg(not(test))]
pub(super) fn execution_command(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    expected_identity: Option<&str>,
) -> Result<Command, String> {
    let current_executable = std::env::current_exe()
        .map_err(|error| format!("could not locate MailSwiftSync launcher: {error}"))?;
    let launch_executable = hardened_executable_path(executable)?;
    let mut command = Command::new(current_executable);
    // The launcher becomes the engine, so it starts from the same minimal
    // environment the engine may see.
    crate::process::apply_engine_environment(&mut command);
    let engine_env = filtered_engine_env(env);
    command
        .arg("--internal-launcher")
        .arg(&launch_executable)
        .args(expected_identity.map(|identity| format!("--expected-identity={identity}")))
        .arg("--")
        .args(args)
        .envs(engine_env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok(command)
}

/// Unit-test binaries do not dispatch through the application CLI. On Unix,
/// use a tiny stdin-gated shell trampoline to exercise the same durable
/// registration/release boundary as the production internal launcher.
#[cfg(all(test, unix))]
pub(super) fn execution_command(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    _expected_identity: Option<&str>,
) -> Result<Command, String> {
    let launch_executable = hardened_executable_path(executable)?;
    let mut command = Command::new("/bin/sh");
    crate::process::apply_engine_environment(&mut command);
    let engine_env = filtered_engine_env(env);
    command
        .arg("-c")
        .arg("IFS= read -r gate || exit 125; [ \"$gate\" = GO ] || exit 125; exec \"$@\"")
        .arg("mailswiftsync-test-launcher")
        .arg(launch_executable)
        .args(args)
        .envs(engine_env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok(command)
}

/// Windows unit-test binaries do not dispatch through the application CLI;
/// keep direct execution there. Native Windows process-containment behavior
/// remains covered by its dedicated runtime qualification jobs.
#[cfg(all(test, not(unix)))]
pub(super) fn execution_command(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    _expected_identity: Option<&str>,
) -> Result<Command, String> {
    let launch_executable = hardened_executable_path(executable)?;
    let mut command = Command::new(launch_executable);
    crate::process::apply_engine_environment(&mut command);
    let engine_env = filtered_engine_env(env);
    command
        .args(args)
        .envs(engine_env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok(command)
}

fn hardened_executable_path(executable: &str) -> Result<String, String> {
    if crate::process::engine_execution_profile()
        != crate::process::EngineExecutionProfile::Hardened
    {
        return Ok(executable.to_owned());
    }
    crate::plan_identity::resolve_executable(executable)
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| format!("hardened execution could not resolve executable {executable:?}"))
}

fn filtered_engine_env<'a>(
    env: &'a [(String, SecretString)],
) -> impl Iterator<Item = (&'a String, &'a str)> + 'a {
    env.iter().filter_map(|(key, value)| {
        let blocked = crate::process::engine_execution_profile()
            == crate::process::EngineExecutionProfile::Hardened
            && matches!(
                key.to_ascii_uppercase().as_str(),
                "PATH" | "PERL5LIB" | "PERL_LOCAL_LIB_ROOT" | "SSL_CERT_FILE" | "SSL_CERT_DIR"
            );
        (!blocked).then_some((key, value.as_str()))
    })
}

pub(super) fn release_engine(
    release_stdin: &mut Option<std::process::ChildStdin>,
) -> Result<(), String> {
    if let Some(mut stdin) = release_stdin.take() {
        stdin
            .write_all(b"GO\n")
            .map_err(|error| format!("could not signal the internal launcher: {error}"))?;
    }
    Ok(())
}

pub(super) fn release_engine_with_limiter(
    release_stdin: &mut Option<std::process::ChildStdin>,
    launch_limiter: Option<(
        &crate::process::ProcessLaunchLimiter,
        &crate::controller::rate_domains::RateDomainPath,
    )>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let Some((limiter, path)) = launch_limiter else {
        return release_engine(release_stdin);
    };
    match limiter.launch_scoped(path, cancel, || release_engine(release_stdin)) {
        Some(result) => result,
        None => {
            // Closing the pipe makes an unreleased gatekeeper exit instead
            // of waiting indefinitely after cancellation or poisoned limiter
            // state. The caller supervises and reaps that child.
            drop(release_stdin.take());
            Err(if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                "engine release cancelled before process start".into()
            } else {
                "engine release refused: process-start limiter unavailable".into()
            })
        }
    }
}

/// Entry point used only by the production binary's internal launcher mode.
/// Arguments are passed as OS strings so executable paths and engine options
/// do not undergo shell parsing or lossy Unicode conversion.
pub(crate) fn run_internal_launcher(arguments: Vec<std::ffi::OsString>) -> i32 {
    let Some((executable, remainder)) = arguments.split_first() else {
        return 125;
    };
    let Some(separator) = remainder.iter().position(|argument| argument == "--") else {
        return 125;
    };
    let expected_identity = match &remainder[..separator] {
        [] => None,
        [argument] => argument
            .to_str()
            .and_then(|value| value.strip_prefix("--expected-identity=")),
        _ => return 125,
    };
    if !remainder[..separator].is_empty() && expected_identity.is_none() {
        return 125;
    }
    let mut release = [0_u8; 3];
    if std::io::stdin().read_exact(&mut release).is_err() || release != *b"GO\n" {
        return 125;
    }
    if let Some(expected_identity) = expected_identity {
        let Some(executable_path) = executable.to_str() else {
            eprintln!("refusing to launch an engine with a non-UTF-8 path");
            return 125;
        };
        if let Err(error) =
            crate::plan_identity::revalidate_executable(executable_path, expected_identity)
        {
            eprintln!("refusing to launch an engine whose identity changed: {error}");
            return 125;
        }
    }
    let mut command = Command::new(executable);
    // Re-apply the allowlist even though the controller already cleared the
    // launcher's environment, so the engine never depends on the caller.
    crate::process::apply_engine_environment(&mut command);
    command
        .args(&remainder[separator + 1..])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // On Unix, replace the gatekeeper image instead of creating a second
    // process. The durable ProcessStarted record therefore continues to
    // identify the actual migration engine, while the session/process group
    // and PR_SET_PDEATHSIG containment survive the exec. Spawning the engine
    // here leaves a dead launcher PID in the ledger after a controller crash,
    // allowing the real child to outlive its recorded ownership.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("could not exec migration engine: {error}");
        125
    }
    #[cfg(not(unix))]
    {
        match command.spawn().and_then(|mut child| child.wait()) {
            Ok(status) => status.code().unwrap_or(1),
            Err(_) => 125,
        }
    }
}

pub(super) struct ProcessRegistrationGuard<'a> {
    tx: &'a mpsc::SyncSender<crate::Event>,
    run_id: String,
    job_id: String,
}

impl Drop for ProcessRegistrationGuard<'_> {
    fn drop(&mut self) {
        if let Err(error) = send_reliable_event(
            self.tx,
            crate::Event::ProcessEnded {
                run_id: self.run_id.clone(),
                job_id: self.job_id.clone(),
            },
        ) {
            eprintln!("reliable process-end event delivery failed: {error}");
        }
    }
}

/// Spawn, retrying briefly while the kernel reports the executable busy
/// (ETXTBSY). A file that was just written can still be open for writing
/// in a child another thread forked but has not yet exec'd, for example an
/// engine installed moments ago. The window is microseconds; a few short
/// retries are enough and every other error is returned at once.
#[cfg(unix)]
pub(super) fn spawn_retrying_busy_executable(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    const ATTEMPTS: u32 = 20;
    const ETXTBSY: i32 = 26;
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(ETXTBSY) && attempt + 1 < ATTEMPTS => {
                attempt += 1;
                thread::sleep(Duration::from_millis(5 * u64::from(attempt)));
            }
            result => return result,
        }
    }
}

/// Windows has no ETXTBSY race of this kind; spawn directly.
#[cfg(not(unix))]
pub(super) fn spawn_retrying_busy_executable(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    command.spawn()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_capture_lines(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    cancel: &AtomicBool,
    secrets: &[SecretString],
    timeout: Duration,
    observer: Option<OutputObserver>,
    durable: Option<(&mpsc::SyncSender<crate::Event>, &str, &str)>,
) -> Result<(ProcessOutcome, Vec<String>, bool), String> {
    let mut command = execution_command(executable, args, env, None)?;
    let mut child = spawn_retrying_busy_executable(&mut command)
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let mut release_stdin = child.stdin.take();
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
    let reader_secrets = crate::process::RevocableReaderSecrets::new(secrets);
    let out_secrets = reader_secrets.clone();
    let out_observer = observer.clone();
    let (out_reader_tx, out_reader_rx) = mpsc::sync_channel(1);
    let out_thread = crate::process::spawn_reader_thread(move || {
        let result = crate::process::collect_revocably_redacted_lines_with_callback(
            stdout,
            &out_secrets,
            |line| {
                if let Some(observer) = &out_observer {
                    observer(line);
                }
            },
        );
        let _ = out_reader_tx.send(result);
    });
    let err_secrets = reader_secrets.clone();
    let err_observer = observer;
    let (err_reader_tx, err_reader_rx) = mpsc::sync_channel(1);
    let err_thread = crate::process::spawn_reader_thread(move || {
        let result = crate::process::collect_revocably_redacted_lines_with_callback(
            stderr,
            &err_secrets,
            |line| {
                if let Some(observer) = &err_observer {
                    observer(line);
                }
            },
        );
        let _ = err_reader_tx.send(result);
    });
    let registration = durable.map(|(tx, run_id, job_id)| {
        register_process(tx, run_id, job_id, executable, &child, cancel)
    });
    let mut registration_error = registration
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    if registration_error.is_none()
        && let Err(error) = release_engine(&mut release_stdin)
    {
        registration_error = Some(format!("engine release failed; child cancelled: {error}"));
    }
    if registration_error.is_some() {
        cancel.store(true, Ordering::Relaxed);
        terminate_process_group(&mut child);
    }
    let _registration_guard = match (durable, registration_error.is_none()) {
        (Some((tx, run_id, job_id)), true) => Some(ProcessRegistrationGuard {
            tx,
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
        }),
        _ => None,
    };
    let debug_supervision = process_supervision_debug_enabled();
    if debug_supervision {
        eprintln!("[process-debug] capture waiting for child {}", child.id());
    }
    let status = wait_with_timeout(&mut child, timeout, cancel).map_err(|error| error.to_string());
    if debug_supervision {
        eprintln!("[process-debug] capture wait returned; joining stdout reader");
    }
    let reader_deadline = std::time::Instant::now() + crate::process::READER_DRAIN_TIMEOUT;
    let stdout_lines = crate::process::receive_revocable_reader_result(
        out_reader_rx,
        out_thread,
        reader_deadline,
        "stdout",
        &reader_secrets,
    );
    let stdout_lines = stdout_lines?.map_err(|error| format!("stdout reader failed: {error}"));
    if debug_supervision {
        eprintln!("[process-debug] stdout reader joined; joining stderr reader");
    }
    let stderr_lines = crate::process::receive_revocable_reader_result(
        err_reader_rx,
        err_thread,
        reader_deadline,
        "stderr",
        &reader_secrets,
    );
    let stderr_lines = stderr_lines?.map_err(|error| format!("stderr reader failed: {error}"));
    if debug_supervision {
        eprintln!("[process-debug] stderr reader joined");
    }
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
    if let Some(error) = registration_error {
        return Err(error);
    }
    if capture_truncated {
        lines.push(
            "[diagnostics truncated; semantic verification continued from the full stream]".into(),
        );
    }
    Ok((status, lines, capture_truncated))
}

pub(crate) fn process_supervision_debug_enabled() -> bool {
    std::env::var_os("MAILSWIFTSYNC_DEBUG_PROCESS_WAIT").is_some_and(|value| value == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn shared_launch_limiter_paces_gatekeeper_release_after_registration() {
        use crate::controller::rate_domains::{RateDomainPath, SideIdentity};

        const WORKERS: usize = 4;
        const STARTS_PER_SECOND: usize = 4;
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-release-rate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let active = directory.join("active-engine");
        let burst = directory.join("burst-detected");
        let source = SideIdentity {
            endpoint: "source.example:993",
            user: "source@example.test",
            tenant: "",
            principal: "",
        };
        let destination = SideIdentity {
            endpoint: "destination.example:993",
            user: "destination@example.test",
            tenant: "",
            principal: "",
        };
        let path = RateDomainPath::new(&source, &destination);
        let limiter = Arc::new(crate::process::ProcessLaunchLimiter::new(STARTS_PER_SECOND));
        let cancel = Arc::new(AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(WORKERS));
        let mut children = Vec::with_capacity(WORKERS);
        let mut releases = Vec::with_capacity(WORKERS);

        for _ in 0..WORKERS {
            let mut child = Command::new("/bin/sh")
                .arg("-c")
                .arg(
                    "IFS= read -r gate; [ \"$gate\" = GO ] || exit 2; \
                     if ! mkdir \"$1\" 2>/dev/null; then : > \"$2\"; \
                     else sleep 0.1; rmdir \"$1\"; fi",
                )
                .arg("mailswiftsync-test-engine")
                .arg(&active)
                .arg(&burst)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            releases.push(child.stdin.take().unwrap());
            children.push(child);
        }

        let releasers = releases
            .into_iter()
            .map(|stdin| {
                let limiter = Arc::clone(&limiter);
                let cancel = Arc::clone(&cancel);
                let start = Arc::clone(&start);
                let path = path.clone();
                thread::spawn(move || {
                    let mut stdin = Some(stdin);
                    start.wait();
                    release_engine_with_limiter(&mut stdin, Some((&limiter, &path)), &cancel)
                })
            })
            .collect::<Vec<_>>();

        for releaser in releasers {
            releaser.join().unwrap().unwrap();
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        assert!(
            !burst.exists(),
            "simultaneously acknowledged gatekeepers must not execute engines in a burst"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn queued_engine_release_observes_cancellation_before_consuming_a_token() {
        use crate::controller::rate_domains::{RateDomainPath, SideIdentity};

        let source = SideIdentity {
            endpoint: "source.example:993",
            user: "source@example.test",
            tenant: "",
            principal: "",
        };
        let destination = SideIdentity {
            endpoint: "destination.example:993",
            user: "destination@example.test",
            tenant: "",
            principal: "",
        };
        let path = RateDomainPath::new(&source, &destination);
        let limiter = Arc::new(crate::process::ProcessLaunchLimiter::new(1));
        let first_cancel = Arc::new(AtomicBool::new(false));
        let second_cancel = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (unblock_tx, unblock_rx) = mpsc::sync_channel(1);
        let first_limiter = Arc::clone(&limiter);
        let first_path = path.clone();
        let first_cancel_for_thread = Arc::clone(&first_cancel);
        let first = thread::spawn(move || {
            first_limiter.launch_scoped(&first_path, &first_cancel_for_thread, || {
                entered_tx.send(()).unwrap();
                unblock_rx.recv().unwrap();
                Ok::<_, ()>(())
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first engine release should enter its bounded closure");

        let (second_entered_tx, second_entered_rx) = mpsc::sync_channel(1);
        let second_limiter = Arc::clone(&limiter);
        let second_cancel_for_thread = Arc::clone(&second_cancel);
        let second_path = path.clone();
        let closure_called = Arc::new(AtomicBool::new(false));
        let closure_called_by_thread = Arc::clone(&closure_called);
        let second = thread::spawn(move || {
            second_entered_tx.send(()).unwrap();
            second_limiter.launch_scoped(&second_path, &second_cancel_for_thread, || {
                closure_called_by_thread.store(true, Ordering::Relaxed);
                Ok::<_, ()>(())
            })
        });
        second_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second engine release should queue behind the shared limiter");
        second_cancel.store(true, Ordering::Relaxed);
        unblock_tx.send(()).unwrap();

        assert_eq!(first.join().unwrap(), Some(Ok(())));
        assert_eq!(second.join().unwrap(), None);
        assert!(!closure_called.load(Ordering::Relaxed));
    }

    #[test]
    fn reader_result_join_is_bounded_when_a_pipe_drainer_does_not_finish() {
        let (sender, receiver) = mpsc::sync_channel::<()>(1);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let handle = thread::spawn(move || {
            let _sender = sender;
            started_tx.send(()).unwrap();
            // The timeout makes a regression to an unbounded join fail this
            // test instead of hanging the complete platform test suite.
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            drop(_sender);
            finished_tx.send(()).unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reader thread should start before its result is awaited");
        let result = crate::process::receive_reader_result(
            receiver,
            handle,
            std::time::Instant::now() + Duration::from_millis(10),
            "stdout",
        );
        assert!(result.is_err());
        assert!(
            finished_rx.try_recv().is_err(),
            "a bounded receive must not join the still-blocked reader"
        );
        release_tx
            .send(())
            .expect("test should release the reader after checking the bound");
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("released reader should finish cleanly");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn capture_returns_when_a_detached_descendant_holds_pipes_open() {
        if crate::plan_identity::resolve_executable("python3").is_none() {
            eprintln!("skipping detached-descendant capture test: python3 is unavailable");
            return;
        }
        let pid_file = std::env::temp_dir().join(format!(
            "mailswiftsync-detached-pipe-{}-{}.pid",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let pid_file_text = pid_file.to_string_lossy().into_owned();
        let command = "python3 -c 'import os, time; child=os.fork(); child and os._exit(0); os.setsid(); open(os.environ[\"MAILSWIFT_SYNC_DETACHED_PID_FILE\"], \"w\").write(str(os.getpid())); time.sleep(7); print(\"secret-canary\", flush=True); time.sleep(30)' & while [ ! -s \"$MAILSWIFT_SYNC_DETACHED_PID_FILE\" ]; do sleep 0.01; done; exit 0";
        let cancel = AtomicBool::new(false);
        let observed = Arc::new(Mutex::new(Vec::<String>::new()));
        let observer_lines = Arc::clone(&observed);
        let observer: OutputObserver = Arc::new(move |line| {
            observer_lines.lock().unwrap().push(line.to_owned());
        });
        let started = std::time::Instant::now();
        let result = run_capture_lines(
            "sh",
            &["-c".into(), command.into()],
            &[(
                "MAILSWIFT_SYNC_DETACHED_PID_FILE".into(),
                SecretString::from(pid_file_text),
            )],
            &cancel,
            &[SecretString::from("secret-canary")],
            Duration::from_secs(2),
            Some(observer),
            None,
        );
        let elapsed = started.elapsed();
        let detached_pid = (0..20).find_map(|_| {
            std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
                .or_else(|| {
                    std::thread::sleep(Duration::from_millis(25));
                    None
                })
        });
        let detached_pid = detached_pid.unwrap_or_else(|| {
            panic!("detached descendant did not publish its PID; capture result: {result:?}")
        });
        // Prove the descendant survived outside the process group supervised
        // for the capture command; otherwise process-group cleanup can close
        // the pipes and make this test pass without exercising revocation.
        // SAFETY: PID was written by the fixture and is killed below.
        let descendant_group = unsafe { libc::getpgid(detached_pid) };
        // SAFETY: same validated test-owned PID.
        let descendant_session = unsafe { libc::getsid(detached_pid) };
        assert_eq!(descendant_group, detached_pid);
        assert_eq!(descendant_session, detached_pid);
        // The descendant writes only after the controller's five-second
        // reader deadline. Its inherited pipe must not keep forwarding output
        // after run_capture_lines has returned.
        std::thread::sleep(Duration::from_secs(3));
        // SAFETY: the test fixture created this process and its PID was read
        // from the private PID file it wrote.
        unsafe {
            libc::kill(detached_pid, libc::SIGKILL);
        }
        assert!(observed.lock().unwrap().is_empty());
        let _ = std::fs::remove_file(&pid_file);
        let error = result.expect_err("detached pipe holder was reported cleanly");
        assert!(
            error.contains("reader was stopped"),
            "revoked pipe drainer did not terminate: {error}"
        );
        assert!(
            elapsed < READER_DRAIN_TIMEOUT + Duration::from_secs(2),
            "capture exceeded its bounded reader deadline: {elapsed:?}"
        );
    }
}
