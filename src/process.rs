use std::{
    fs::OpenOptions,
    io::Read,
    path::Path,
    process::{Child, Command},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

use fs2::FileExt;

use crate::{core, credentials::SecretString};

const MAX_SUBPROCESS_LINE_BYTES: usize = 64 * 1024;
const MAX_CAPTURED_OUTPUT_LINES: usize = 8 * 1024;
const MAX_CAPTURED_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct CapturedOutput {
    pub(crate) lines: Vec<String>,
    pub(crate) truncated: bool,
}

/// Exclusive ownership of the application state workspace. Recovery must
/// never run while another MailSwiftSync instance may still own processes.
#[derive(Debug)]
pub(crate) struct InstanceLock(std::fs::File);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Owns the external engine's descendant lifetime on platforms that provide a
/// kernel-level process container. On Windows, closing this handle (including
/// because the controller crashes) terminates every process assigned to the
/// job. Unix uses its existing session/process-group supervision instead.
#[derive(Debug)]
pub(crate) struct ChildSupervisor {
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl Drop for ChildSupervisor {
    fn drop(&mut self) {
        #[cfg(windows)]
        if !self.job.is_null() {
            // SAFETY: the handle was returned by CreateJobObjectW and is
            // owned exclusively by this guard.
            unsafe {
                let _ = windows_sys::Win32::Foundation::CloseHandle(self.job);
            }
        }
    }
}

pub(crate) fn attach_child_supervisor(child: &Child) -> std::io::Result<ChildSupervisor> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            unsafe {
                let _ = windows_sys::Win32::Foundation::CloseHandle(job);
            }
            return Err(std::io::Error::last_os_error());
        }
        let assigned = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as _) };
        if assigned == 0 {
            unsafe {
                let _ = windows_sys::Win32::Foundation::CloseHandle(job);
            }
            return Err(std::io::Error::last_os_error());
        }
        return Ok(ChildSupervisor { job });
    }
    #[cfg(not(windows))]
    {
        let _ = child;
        Ok(ChildSupervisor {})
    }
}

pub(crate) fn acquire_instance_lock(state_path: &Path) -> Result<InstanceLock, String> {
    let lock_path = state_path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("could not open application lock: {error}"))?;
    crate::credentials::restrict_file_permissions(&lock_path).map_err(|error| error.to_string())?;
    if file.try_lock_exclusive().is_err() {
        // Explicitly close a denied contender before returning. This keeps a
        // failed second open from retaining an open descriptor on platforms
        // whose advisory-lock behavior is sensitive to descriptor lifetime.
        drop(file);
        return Err("Another MailSwiftSync instance holds the project database. Close the existing window before opening this workspace; do not delete the lock file while it may be running.".to_owned());
    }
    Ok(InstanceLock(file))
}

#[cfg(test)]
pub(crate) fn collect_redacted_lines<R: Read>(
    reader: R,
    secrets: &[SecretString],
) -> std::io::Result<CapturedOutput> {
    collect_redacted_lines_with_callback(reader, secrets, |_| {})
}

/// Drain and redact a stream while giving a separate consumer every complete
/// line. The callback is intentionally invoked before bounded diagnostic
/// retention, allowing semantic reducers to process arbitrarily large engine
/// reports in constant memory.
pub(crate) fn collect_redacted_lines_with_callback<R: Read, F: FnMut(&str)>(
    reader: R,
    secrets: &[SecretString],
    mut callback: F,
) -> std::io::Result<CapturedOutput> {
    let mut lines = Vec::new();
    let mut retained_bytes: usize = 0;
    let mut truncated = false;
    for_each_lossy_line(reader, |mut line| {
        for secret in secrets {
            if !secret.is_empty() {
                line = line.replace(secret.as_str(), "[REDACTED]");
            }
        }
        callback(&line);
        let within_limits = lines.len() < MAX_CAPTURED_OUTPUT_LINES
            && retained_bytes.saturating_add(line.len()) <= MAX_CAPTURED_OUTPUT_BYTES;
        if within_limits {
            retained_bytes = retained_bytes.saturating_add(line.len());
            lines.push(line);
        } else if !truncated {
            lines.push(format!(
                "[diagnostics truncated after {MAX_CAPTURED_OUTPUT_LINES} lines or {MAX_CAPTURED_OUTPUT_BYTES} bytes]"
            ));
            truncated = true;
        }
    })?;
    Ok(CapturedOutput { lines, truncated })
}

#[cfg(test)]
pub(crate) fn read_lossy_lines<R: Read>(reader: R) -> Vec<String> {
    let mut lines = Vec::new();
    for_each_lossy_line(reader, |line| lines.push(line)).expect("test reader should not fail");
    lines
}

/// Consume subprocess output incrementally while tolerating malformed UTF-8.
pub(crate) fn for_each_lossy_line<R: Read, F: FnMut(String)>(
    mut reader: R,
    mut callback: F,
) -> std::io::Result<()> {
    let mut chunk = [0_u8; 8192];
    let mut line = Vec::with_capacity(8192);
    let mut line_started = false;
    let mut truncated = false;
    loop {
        let bytes_read = reader.read(&mut chunk)?;
        if bytes_read == 0 {
            if line_started {
                emit_lossy_line(&line, truncated, &mut callback);
            }
            return Ok(());
        }
        for byte in &chunk[..bytes_read] {
            if *byte == b'\n' {
                emit_lossy_line(&line, truncated, &mut callback);
                line.clear();
                line_started = false;
                truncated = false;
            } else {
                line_started = true;
                if line.len() < MAX_SUBPROCESS_LINE_BYTES {
                    line.push(*byte);
                } else {
                    truncated = true;
                }
            }
        }
    }
}

fn emit_lossy_line<F: FnMut(String)>(line: &[u8], truncated: bool, callback: &mut F) {
    let mut text = String::from_utf8_lossy(line)
        .trim_end_matches('\r')
        .to_owned();
    if truncated {
        text.push_str(" [line truncated by MailSwiftSync]");
    }
    callback(text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct FailingReader {
        emitted: bool,
    }

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.emitted {
                Err(io::Error::other("synthetic reader failure"))
            } else {
                self.emitted = true;
                buffer[..6].copy_from_slice(b"first\n");
                Ok(6)
            }
        }
    }

    #[test]
    fn lossy_line_reader_reports_pipe_errors_after_emitting_lines() {
        let mut lines = Vec::new();
        let result = for_each_lossy_line(FailingReader { emitted: false }, |line| {
            lines.push(line);
        });

        assert_eq!(lines, ["first"]);
        assert_eq!(
            result.unwrap_err().kind(),
            io::ErrorKind::Other,
            "reader errors must not be mistaken for clean EOF"
        );
    }

    #[test]
    fn lossy_line_reader_bounds_a_single_unterminated_line() {
        let input = vec![b'x'; MAX_SUBPROCESS_LINE_BYTES * 2];
        let mut lines = Vec::new();

        for_each_lossy_line(std::io::Cursor::new(input), |line| lines.push(line)).unwrap();

        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with("[line truncated by MailSwiftSync]"));
        assert!(lines[0].len() < MAX_SUBPROCESS_LINE_BYTES + 64);
    }

    #[test]
    fn captured_output_retains_a_bounded_prefix_and_drains_the_reader() {
        let input = (0..(MAX_CAPTURED_OUTPUT_LINES + 100))
            .map(|index| format!("line-{index}\n"))
            .collect::<String>();
        let output = collect_redacted_lines(std::io::Cursor::new(input), &[]).unwrap();

        assert_eq!(output.lines.len(), MAX_CAPTURED_OUTPUT_LINES + 1);
        assert!(
            output
                .lines
                .last()
                .unwrap()
                .contains("diagnostics truncated")
        );
        assert!(output.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn wait_reports_operator_cancellation_as_a_typed_outcome() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let cancel = AtomicBool::new(true);

        let outcome = wait_with_timeout(&mut child, Duration::from_secs(5), &cancel).unwrap();

        assert_eq!(outcome.exit_code, None);
        assert!(outcome.cancelled);
        assert!(!outcome.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn wait_reports_timeout_as_a_typed_outcome() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let cancel = AtomicBool::new(false);

        let outcome = wait_with_timeout(&mut child, Duration::from_millis(1), &cancel).unwrap();

        assert_eq!(outcome.exit_code, None);
        assert!(!outcome.cancelled);
        assert!(outcome.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn leader_exit_does_not_hide_a_live_descendant() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & exit 0"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let actual_group = child.id();
        let cancel = AtomicBool::new(false);

        let result = wait_with_timeout(&mut child, Duration::from_secs(5), &cancel);

        assert!(
            result.is_err(),
            "a live descendant must not look like success"
        );
        assert!(!process_group_exists(actual_group));
    }

    #[cfg(windows)]
    #[test]
    fn job_supervisor_kills_the_engine_tree_when_dropped() {
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "ping.exe -t 127.0.0.1"]);
        let mut child = command.spawn().expect("Windows command shell must exist");
        let supervisor = attach_child_supervisor(&child)
            .expect("engine child must be assignable to a kill-on-close job");

        thread::sleep(Duration::from_millis(200));
        assert!(
            child.try_wait().unwrap().is_none(),
            "test engine should still be running before supervisor close"
        );
        drop(supervisor);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        if !exited {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(exited, "closing the job must terminate the engine process");
    }
}

#[derive(Debug)]
struct TokenBucketState {
    tokens: f64,
    last_refill: Instant,
}

/// Shared admission control for batch engine processes.
///
/// The external engine owns the IMAP connection, so the controller cannot
/// meter individual messages directly. Batch command construction divides
/// configured imapsync message/byte ceilings across workers; this bucket
/// smooths process starts so a batch does not authenticate every mailbox at
/// once. The limiter is intentionally shared by all workers.
pub(crate) struct ProcessLaunchLimiter {
    rate_per_second: f64,
    state: Mutex<TokenBucketState>,
}

/// Result of supervising an external process. Engine adapters can interpret
/// the exit code without having to infer cancellation or timeout from an
/// error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessOutcome {
    pub(crate) exit_code: Option<i32>,
    pub(crate) cancelled: bool,
    pub(crate) timed_out: bool,
}

impl ProcessLaunchLimiter {
    pub(crate) fn new(starts_per_second: usize) -> Self {
        let rate_per_second = starts_per_second.max(1) as f64;
        Self {
            rate_per_second,
            state: Mutex::new(TokenBucketState {
                tokens: 1.0,
                last_refill: Instant::now(),
            }),
        }
    }

    pub(crate) fn acquire(&self, cancel: &AtomicBool) -> bool {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let wait = {
                let mut state = match self.state.lock() {
                    Ok(state) => state,
                    Err(_) => return false,
                };
                let now = Instant::now();
                let elapsed = now.duration_since(state.last_refill).as_secs_f64();
                state.tokens = (state.tokens + elapsed * self.rate_per_second).min(1.0);
                state.last_refill = now;
                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return true;
                }
                Duration::from_secs_f64((1.0 - state.tokens) / self.rate_per_second)
            };
            thread::sleep(wait.min(Duration::from_millis(100)));
        }
    }
}

pub(crate) fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
    cancel: &AtomicBool,
) -> std::io::Result<ProcessOutcome> {
    #[cfg(unix)]
    let process_group = child.id();
    let started = Instant::now();
    loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                // A wait error must not leave an owned migration process
                // running without a controller.  Clean up before returning
                // the original error so callers can safely join pipe readers.
                terminate_process_group(child);
                wait_for_graceful_exit(child, Duration::from_secs(5));
                return Err(error);
            }
        };
        if let Some(status) = status {
            #[cfg(unix)]
            if process_group_exists(process_group) {
                terminate_process_group_id(process_group);
                if !wait_for_process_group_exit(process_group, Duration::from_secs(5)) {
                    return Err(std::io::Error::other(
                        "migration leader exited while a descendant process group remained",
                    ));
                }
                if cancel.load(Ordering::Relaxed) {
                    return Ok(ProcessOutcome {
                        exit_code: None,
                        cancelled: true,
                        timed_out: false,
                    });
                }
                return Err(std::io::Error::other(
                    "migration leader exited while a descendant process remained",
                ));
            }
            return Ok(ProcessOutcome {
                exit_code: status.code(),
                cancelled: false,
                timed_out: false,
            });
        }
        if cancel.load(Ordering::Relaxed) {
            terminate_process_group(child);
            wait_for_graceful_exit(child, Duration::from_secs(5));
            return Ok(ProcessOutcome {
                exit_code: None,
                cancelled: true,
                timed_out: false,
            });
        }
        if started.elapsed() >= timeout {
            terminate_process_group(child);
            wait_for_graceful_exit(child, Duration::from_secs(5));
            return Ok(ProcessOutcome {
                exit_code: None,
                cancelled: false,
                timed_out: true,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_graceful_exit(child: &mut Child, grace: Duration) {
    let started = Instant::now();
    while started.elapsed() < grace {
        match child.try_wait() {
            Ok(Some(_)) => {
                #[cfg(unix)]
                {
                    // The session leader can exit while a wrapper or helper
                    // descendant remains in the migration process group.
                    // Keep the grace period alive for the group, not merely
                    // for the leader, so cancellation cannot declare cleanup
                    // complete while a descendant still has mailbox access.
                    if !process_group_exists(child.id()) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                #[cfg(not(unix))]
                return;
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    force_kill_process_group(child);
    let _ = child.wait();
}

#[cfg(unix)]
fn process_group_exists(process_group: u32) -> bool {
    let result = unsafe { libc::kill(-(process_group as libc::pid_t), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn terminate_process_group_id(process_group: u32) {
    unsafe {
        let _ = libc::kill(-(process_group as libc::pid_t), libc::SIGTERM);
    }
}

#[cfg(unix)]
fn force_kill_process_group_id(process_group: u32) {
    unsafe {
        let _ = libc::kill(-(process_group as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(unix)]
fn wait_for_process_group_exit(process_group: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !process_group_exists(process_group) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if process_group_exists(process_group) {
        force_kill_process_group_id(process_group);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if !process_group_exists(process_group) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    !process_group_exists(process_group)
}

pub(crate) fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Each migration gets its own session so cancellation cannot leave a
        // shell/wrapper descendant running against the destination.
        unsafe {
            command.pre_exec(|| {
                #[cfg(target_os = "linux")]
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

pub(crate) fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    terminate_process_group_id(child.id());
    #[cfg(not(unix))]
    let _ = child.kill();
}

fn force_kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    force_kill_process_group_id(child.id());
    #[cfg(not(unix))]
    let _ = child.kill();
}

/// Terminate a process group when its durable platform identity still matches.
/// Identity is checked both before SIGTERM and immediately before escalation
/// to SIGKILL; a recycled PID or process group is never signalled.
pub(crate) fn terminate_recorded_process_group(process: &core::ActiveProcess) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if !recorded_process_matches(process) {
            return;
        }
        let process_group = process.process_group.unwrap_or(process.pid);
        terminate_process_group_id(process_group);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !recorded_process_matches(process) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if recorded_process_matches(process) {
            force_kill_process_group_id(process_group);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = process;
}

/// Terminate a child group created by this process. This helper is used only
/// for an already-owned live child; startup recovery must use the identity-
/// checked function above.
#[cfg(test)]
pub(crate) fn terminate_process_group_by_pid(pid: u32) {
    #[cfg(unix)]
    {
        terminate_process_group_id(pid);
        thread::sleep(Duration::from_secs(2));
        force_kill_process_group_id(pid);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(target_os = "linux")]
pub(crate) fn process_identity(pid: u32) -> Option<(u64, u32, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let process_group = fields.get(2)?.parse().ok()?;
    let session_id = fields.get(3)?.parse().ok()?;
    let start_ticks = fields.get(19)?.parse().ok()?;
    Some((start_ticks, process_group, session_id))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_identity(pid: u32) -> Option<(u64, u32, u32)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
        )
    };
    if size != std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let session_id = unsafe { libc::getsid(pid as libc::pid_t) };
    if session_id < 0 {
        return None;
    }
    let start_ticks = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)?;
    Some((start_ticks, info.pbi_pgid, session_id as u32))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn process_identity(_pid: u32) -> Option<(u64, u32, u32)> {
    None
}

pub(crate) fn recorded_process_matches(process: &core::ActiveProcess) -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let Some((start_ticks, process_group, session_id)) = process_identity(process.pid) else {
            return false;
        };
        process.start_ticks == Some(start_ticks)
            && process.process_group == Some(process_group)
            && process.session_id == Some(session_id)
            && process_group == process.pid
            && session_id == process.pid
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = process;
        false
    }
}
