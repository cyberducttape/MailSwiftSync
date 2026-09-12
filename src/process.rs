use std::{
    io::Read,
    process::{Child, Command, ExitStatus},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::core;

const MAX_SUBPROCESS_LINE_BYTES: usize = 64 * 1024;

pub(crate) fn collect_redacted_lines<R: Read>(
    reader: R,
    secrets: &[String],
) -> std::io::Result<Vec<String>> {
    let mut lines = Vec::new();
    for_each_lossy_line(reader, |mut line| {
        for secret in secrets {
            if !secret.is_empty() {
                line = line.replace(secret, "[REDACTED]");
            }
        }
        lines.push(line);
    })?;
    Ok(lines)
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
) -> std::io::Result<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if cancel.load(Ordering::Relaxed) {
            terminate_process_group(child);
            wait_for_graceful_exit(child, Duration::from_secs(5));
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled by operator",
            ));
        }
        if started.elapsed() >= timeout {
            terminate_process_group(child);
            wait_for_graceful_exit(child, Duration::from_secs(5));
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "migration exceeded its configured execution timeout",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_graceful_exit(child: &mut Child, grace: Duration) {
    let started = Instant::now();
    while started.elapsed() < grace {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    force_kill_process_group(child);
    let _ = child.wait();
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

fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as libc::pid_t);
        unsafe {
            let _ = libc::kill(process_group, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
}

fn force_kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as libc::pid_t);
        unsafe {
            let _ = libc::kill(process_group, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
}

pub(crate) fn terminate_recorded_process_group(pid: u32) {
    #[cfg(unix)]
    {
        let process_group = -(pid as libc::pid_t);
        unsafe {
            let _ = libc::kill(process_group, libc::SIGTERM);
        }
        thread::sleep(Duration::from_secs(2));
        unsafe {
            let _ = libc::kill(process_group, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(target_os = "linux")]
pub(crate) fn linux_process_identity(pid: u32) -> Option<(u64, u32, u32)> {
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

#[cfg(not(target_os = "linux"))]
pub(crate) fn linux_process_identity(_pid: u32) -> Option<(u64, u32, u32)> {
    None
}

pub(crate) fn recorded_process_matches(process: &core::ActiveProcess) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Some((start_ticks, process_group, session_id)) = linux_process_identity(process.pid)
        else {
            return false;
        };
        process.start_ticks == Some(start_ticks)
            && process.process_group == Some(process_group)
            && process.session_id == Some(session_id)
            && process_group == process.pid
            && session_id == process.pid
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = process;
        false
    }
}
