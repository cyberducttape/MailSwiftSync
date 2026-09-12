use std::{
    io::{BufRead, BufReader, Read},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) fn collect_redacted_lines<R: Read>(reader: R, secrets: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    for_each_lossy_line(reader, |mut line| {
        for secret in secrets {
            if !secret.is_empty() {
                line = line.replace(secret, "[REDACTED]");
            }
        }
        lines.push(line);
    });
    lines
}

#[cfg(test)]
pub(crate) fn read_lossy_lines<R: Read>(reader: R) -> Vec<String> {
    let mut lines = Vec::new();
    for_each_lossy_line(reader, |line| lines.push(line));
    lines
}

/// Consume subprocess output incrementally while tolerating malformed UTF-8.
pub(crate) fn for_each_lossy_line<R: Read, F: FnMut(String)>(reader: R, mut callback: F) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let bytes_read = match reader.read_until(b'\n', &mut buffer) {
            Ok(bytes_read) => bytes_read,
            Err(_) => break,
        };
        if bytes_read == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buffer)
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        callback(line);
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
