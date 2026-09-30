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
        let base = crate::core::provider_intelligence::ProviderErrorClassifier::classify(
            "generic",
            crate::controller::failure::control_error_text(error),
        )
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
    let source = canonical_provider_endpoint(
        &form.profile.source_host,
        &form.profile.source_port,
        &form.profile.source_tls,
    );
    let destination = canonical_provider_endpoint(
        &form.profile.destination_host,
        &form.profile.destination_port,
        crate::effective_destination_tls(&form.profile.destination_tls),
    );
    format!("{source}|{destination}")
}

fn canonical_provider_endpoint(host: &str, configured_port: &str, tls_mode: &str) -> String {
    let default_port = crate::default_imap_port(tls_mode);
    let fallback = || {
        format!(
            "invalid:{}:{}",
            host.trim().to_ascii_lowercase(),
            configured_port.trim()
        )
    };
    let Ok((host, embedded_port)) = crate::endpoint::parts(host, default_port) else {
        return fallback();
    };
    let port = if configured_port.trim().is_empty() {
        embedded_port
    } else {
        match configured_port.trim().parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => return fallback(),
        }
    };
    let host = match host.parse::<std::net::IpAddr>() {
        Ok(address) => address.to_string(),
        Err(_) => host.trim_end_matches('.').to_ascii_lowercase(),
    };
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
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
    pub(crate) diagnostic_logger: Option<Arc<crate::DiagnosticLogger>>,
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
        diagnostic_logger,
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
            let diagnostic_logger = diagnostic_logger.clone();
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
                    diagnostic_logger,
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
    use super::{AdaptiveProviderLimiter, provider_scope_key};
    use std::sync::atomic::AtomicBool;

    fn form(
        source_host: &str,
        source_port: &str,
        destination_host: &str,
        destination_port: &str,
    ) -> crate::Form {
        let mut form = crate::Form::default();
        form.profile.source_host = source_host.into();
        form.profile.source_port = source_port.into();
        form.profile.destination_host = destination_host.into();
        form.profile.destination_port = destination_port.into();
        form
    }

    #[test]
    fn provider_scope_canonicalizes_equivalent_host_and_port_forms() {
        let implicit = form("imap.example.com", "", "dest.example.com", "");
        let explicit = form("IMAP.EXAMPLE.COM.", "993", "dest.example.com:993", "");
        let host_embedded = form("imap.example.com:993", "", "DEST.EXAMPLE.COM", "993");
        let key = provider_scope_key(&implicit);

        assert_eq!(key, provider_scope_key(&explicit));
        assert_eq!(key, provider_scope_key(&host_embedded));
        assert_eq!(key, "imap.example.com:993|dest.example.com:993");
    }

    #[test]
    fn provider_scope_uses_tls_specific_defaults_and_configured_port_precedence() {
        let implicit_tls = form("imap.example.com", "", "dest.example.com", "");
        let mut starttls = form("imap.example.com", "", "dest.example.com", "");
        starttls.profile.source_tls = "starttls".into();
        starttls.profile.destination_tls = "starttls".into();
        assert_ne!(
            provider_scope_key(&implicit_tls),
            provider_scope_key(&starttls)
        );
        assert_eq!(
            provider_scope_key(&starttls),
            "imap.example.com:143|dest.example.com:143"
        );

        let configured_overrides_embedded =
            form("imap.example.com:143", "993", "dest.example.com:143", "993");
        assert_eq!(
            provider_scope_key(&implicit_tls),
            provider_scope_key(&configured_overrides_embedded)
        );
    }

    #[test]
    fn provider_scope_canonicalizes_idn_and_ipv6_addresses() {
        let unicode = form("mail.bücher.example", "993", "[2001:0db8::1]", "993");
        let ascii = form("mail.xn--bcher-kva.example.", "", "[2001:db8::1]", "");
        assert_eq!(provider_scope_key(&unicode), provider_scope_key(&ascii));
    }

    #[test]
    fn provider_scope_keeps_distinct_effective_ports_separate() {
        let standard = form("imap.example.com", "", "dest.example.com", "");
        let alternate = form("imap.example.com", "1993", "dest.example.com", "");
        assert_ne!(
            provider_scope_key(&standard),
            provider_scope_key(&alternate)
        );
    }

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

    #[test]
    fn mixed_disconnect_and_capacity_error_activates_long_provider_cooldown() {
        let limiter = AdaptiveProviderLimiter::new();
        limiter.observe_failure("provider-pair", "connection closed: too many connections");
        limiter.observe_failure("network-only", "connection closed by remote host");

        let state = limiter.state.lock().unwrap();
        let now = std::time::Instant::now();
        assert!(state.get("provider-pair").is_some_and(|cooldown| {
            cooldown.blocked_until.duration_since(now) >= std::time::Duration::from_secs(29)
        }));
        assert!(!state.contains_key("network-only"));
    }

    #[test]
    fn presentation_tail_cannot_change_provider_cooldown_duration() {
        let limiter = AdaptiveProviderLimiter::new();
        limiter.observe_failure(
            "provider-pair",
            "too many connections; recent output: HTTP/1.1 429 Too Many Requests",
        );

        let state = limiter.state.lock().unwrap();
        let remaining = state
            .get("provider-pair")
            .expect("primary capacity signal schedules a cooldown")
            .blocked_until
            .duration_since(std::time::Instant::now());
        assert!(remaining >= std::time::Duration::from_secs(29));
        assert!(remaining < std::time::Duration::from_secs(60));
    }
}
