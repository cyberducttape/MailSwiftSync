use super::batch::BatchExecutionMode;
use super::batch_work_item::{BatchWorkerContext, process_batch_work_items, send_job_finished};
use crate::{
    Event, StreamOutcome,
    bulk_import::BulkJob,
    core,
    process::ProcessLaunchLimiter,
    runner::{ResolvedImapsyncIdentity, resolve_imapsync_identity, send_reliable_event},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const BATCH_PROCESS_STARTS_PER_SECOND: usize = 2;
const MAX_BATCH_PENDING_EVENTS: usize = 4_096;
const MAX_ADAPTIVE_PROVIDER_KEYS: usize = 4_096;

type BatchWorkItem = (usize, String, String, Option<String>, BulkJob);
pub(crate) type OAuthRefreshLocks = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;

struct ProviderCooldown {
    blocked_until: Instant,
    consecutive_capacity_failures: u8,
}

/// Shared, endpoint-scoped capacity control for one batch wave.
///
/// This reacts only to observed capacity/rate-limit failures. It deliberately
/// does not encode undocumented provider quotas. A cooldown affects later
/// launches for the same source/destination endpoint pair, while unrelated
/// provider pairs continue to make progress.
pub(crate) struct AdaptiveProviderLimiter {
    state: Mutex<HashMap<String, ProviderCooldown>>,
}

impl AdaptiveProviderLimiter {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn wait(&self, key: &str, cancel: &AtomicBool) -> bool {
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
                match state.get(key).map(|value| value.blocked_until) {
                    Some(blocked_until) if blocked_until > now => blocked_until - now,
                    Some(_) => {
                        state.remove(key);
                        return true;
                    }
                    None => return true,
                }
            };
            thread::sleep(wait.min(Duration::from_millis(100)));
        }
    }

    pub(crate) fn observe_failure(&self, key: &str, error: &str) {
        if crate::controller::failure::classify_failure(error)
            != crate::controller::failure::FailureClass::Capacity
        {
            return;
        }
        let base =
            crate::core::provider_intelligence::ProviderErrorClassifier::classify("generic", error)
                .suggested_retry_delay()
                .unwrap_or(Duration::from_secs(5));
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        let now = Instant::now();
        state.retain(|_, value| value.blocked_until > now);
        if state.len() >= MAX_ADAPTIVE_PROVIDER_KEYS && !state.contains_key(key) {
            return;
        }
        let entry = state.entry(key.to_owned()).or_insert(ProviderCooldown {
            blocked_until: now,
            consecutive_capacity_failures: 0,
        });
        let multiplier = 1u32 << entry.consecutive_capacity_failures.min(5);
        let cooldown = base
            .saturating_mul(multiplier)
            .min(Duration::from_secs(120));
        entry.blocked_until = entry.blocked_until.max(now + cooldown);
        entry.consecutive_capacity_failures = entry.consecutive_capacity_failures.saturating_add(1);
    }
}

pub(crate) fn provider_scope_key(form: &crate::Form) -> String {
    format!(
        "{}:{}|{}:{}",
        form.profile.source_host.trim().to_ascii_lowercase(),
        form.profile.source_port.trim(),
        form.profile.destination_host.trim().to_ascii_lowercase(),
        form.profile.destination_port.trim(),
    )
}

pub(crate) struct BatchWorkerLaunch {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) receiver: mpsc::Receiver<Event>,
}

/// Immutable inputs for one batch execution. Keeping queue identity, retry
/// policy, and work items together prevents positional argument drift between
/// the controller and worker coordinator.
pub(crate) struct BatchExecutionContext {
    pub(crate) concurrency: usize,
    pub(crate) mode: BatchExecutionMode,
    pub(crate) retry_count: usize,
    pub(crate) job_count: usize,
    pub(crate) queue_job_ids: Vec<String>,
    pub(crate) child_run_ids: Vec<String>,
    pub(crate) queue_checkpoints: Vec<Option<String>>,
    pub(crate) batch_project_id: String,
    pub(crate) batch_run_id: String,
    pub(crate) jobs: Vec<BulkJob>,
    pub(crate) verification_state_path: Option<std::path::PathBuf>,
}

/// Create the bounded event channel, cancellation token, and batch worker as
/// one controller-owned operation. Callers only retain the handles needed to
/// render state and request cancellation; channel sizing and worker startup
/// policy do not leak into the egui composition root.
pub(crate) fn launch_batch_worker(context: BatchExecutionContext) -> BatchWorkerLaunch {
    let (tx, receiver) = mpsc::sync_channel(MAX_BATCH_PENDING_EVENTS);
    let cancel = Arc::new(AtomicBool::new(false));
    spawn_batch_worker(context, tx, Arc::clone(&cancel));
    BatchWorkerLaunch { cancel, receiver }
}

pub(crate) fn spawn_batch_worker(
    context: BatchExecutionContext,
    tx: mpsc::SyncSender<Event>,
    cancel: Arc<AtomicBool>,
) {
    let BatchExecutionContext {
        concurrency,
        mode,
        retry_count,
        job_count,
        queue_job_ids,
        child_run_ids,
        queue_checkpoints,
        batch_project_id,
        batch_run_id,
        jobs,
        verification_state_path,
    } = context;
    let launch_limiter = Arc::new(ProcessLaunchLimiter::new(BATCH_PROCESS_STARTS_PER_SECOND));
    let provider_limiter = Arc::new(AdaptiveProviderLimiter::new());
    let oauth_refresh_locks: OAuthRefreshLocks = Arc::new(Mutex::new(HashMap::new()));
    thread::spawn(move || {
        let failed = Arc::new(AtomicBool::new(false));
        let terminal_jobs = Arc::new(Mutex::new(HashSet::new()));
        // Resolve each distinct imapsync executable once before any mailbox
        // process starts. Every child receives the same immutable identity;
        // there is no first-wave subscriber race and no lossy metadata send.
        let mut resolved_imapsync = HashMap::<String, ResolvedImapsyncIdentity>::new();
        for job in &jobs {
            let form = job.form();
            if form.engine() == core::Engine::ImapSync {
                let executable = form.profile.imapsync_path.clone();
                resolved_imapsync
                    .entry(executable.clone())
                    .or_insert_with(|| resolve_imapsync_identity(&executable));
            }
        }
        let resolved_imapsync = Arc::new(resolved_imapsync);
        // Keep only a small number of full job plans in flight.  In
        // particular, do not eagerly enqueue hundreds of Forms (which
        // may contain credential material) before workers have even
        // started.  The producer runs in this coordinator thread, so a
        // bounded queue applies backpressure without blocking the UI.
        let queue_capacity = concurrency.saturating_mul(2).max(1);
        let (job_tx, job_rx): (
            crossbeam_channel::Sender<BatchWorkItem>,
            crossbeam_channel::Receiver<BatchWorkItem>,
        ) = crossbeam_channel::bounded(queue_capacity);
        let mut workers = Vec::with_capacity(concurrency);
        for _ in 0..concurrency {
            let job_rx = job_rx.clone();
            let failed = Arc::clone(&failed);
            let terminal_jobs = Arc::clone(&terminal_jobs);
            let tx = tx.clone();
            let cancel = Arc::clone(&cancel);
            let launch_limiter = Arc::clone(&launch_limiter);
            let provider_limiter = Arc::clone(&provider_limiter);
            let batch_project_id = batch_project_id.clone();
            let batch_run_id = batch_run_id.clone();
            let resolved_imapsync = Arc::clone(&resolved_imapsync);
            let oauth_refresh_locks = Arc::clone(&oauth_refresh_locks);
            let verification_state_path = verification_state_path.clone();
            workers.push(thread::spawn(move || {
                process_batch_work_items(BatchWorkerContext {
                    concurrency,
                    mode,
                    retry_count,
                    job_rx,
                    tx,
                    cancel,
                    failed,
                    terminal_jobs,
                    launch_limiter,
                    provider_limiter,
                    batch_project_id,
                    batch_run_id,
                    resolved_imapsync,
                    oauth_refresh_locks,
                    verification_state_path,
                });
            }));
        }
        drop(job_rx);

        let mut enqueue_failed = false;
        for (index, job) in jobs.into_iter().enumerate() {
            let Some(job_id) = queue_job_ids.get(index).cloned() else {
                failed.store(true, Ordering::Relaxed);
                enqueue_failed = true;
                break;
            };
            let Some(child_run_id) = child_run_ids.get(index).cloned() else {
                failed.store(true, Ordering::Relaxed);
                enqueue_failed = true;
                break;
            };
            if job_tx
                .send((
                    index,
                    job_id,
                    child_run_id,
                    queue_checkpoints.get(index).cloned().unwrap_or_default(),
                    job,
                ))
                .is_err()
            {
                failed.store(true, Ordering::Relaxed);
                enqueue_failed = true;
                break;
            }
        }
        drop(job_tx);

        let mut worker_panicked = false;
        for worker in workers {
            if worker.join().is_err() {
                worker_panicked = true;
                failed.store(true, Ordering::Relaxed);
            }
        }
        if enqueue_failed {
            let _ = tx.send(Event::Line(
                    "Batch work queue disconnected before all jobs were admitted; unresolved jobs require review before retrying."
                        .into(),
                ));
        }
        if worker_panicked {
            let unresolved = terminal_jobs
                .lock()
                .map(|terminal| {
                    (0..job_count)
                        .filter(|index| !terminal.contains(index))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|_| (0..job_count).collect());
            for index in unresolved {
                let Some(job_id) = queue_job_ids.get(index).cloned() else {
                    continue;
                };
                let Some(child_run_id) = child_run_ids.get(index).cloned() else {
                    continue;
                };
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!(
                        "[{}] worker stopped unexpectedly; job moved to Attention",
                        index + 1
                    ),
                });
                let _ = tx.send(Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "Attention".into(),
                });
                if let Err(error) = send_job_finished(
                    &tx,
                    job_id,
                    child_run_id,
                    "attention".into(),
                    "worker stopped unexpectedly".into(),
                    None,
                ) {
                    eprintln!("reliable batch terminal event delivery failed: {error}");
                }
            }
            let _ = tx.send(Event::Line(
                    "A batch worker stopped unexpectedly; unresolved jobs require review before retrying."
                        .into(),
                ));
        }
        if let Err(error) = send_reliable_event(
            &tx,
            Event::Finished(if cancel.load(Ordering::Relaxed) {
                Err("batch cancelled".into())
            } else if failed.load(Ordering::Relaxed) {
                Err("one or more batch jobs failed".into())
            } else {
                Ok(StreamOutcome::Completed)
            }),
        ) {
            eprintln!("reliable batch terminal event delivery failed: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::AdaptiveProviderLimiter;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn adaptive_provider_limiter_scopes_capacity_and_honors_cancellation() {
        let limiter = AdaptiveProviderLimiter::new();
        let key = "imap.gmail.com:993|imap.destination.example:993";
        limiter.observe_failure(key, "too many requests");
        let state = limiter.state.lock().unwrap();
        assert!(
            state
                .get(key)
                .is_some_and(|cooldown| cooldown.blocked_until > std::time::Instant::now())
        );
        drop(state);

        let cancel = AtomicBool::new(true);
        assert!(!limiter.wait(key, &cancel));
        assert!(!limiter.wait("other-provider:993|other-destination:993", &cancel));
    }
}
