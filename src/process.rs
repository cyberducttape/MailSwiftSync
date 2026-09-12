use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

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
