//! Batch scheduling: ready queue → rate-domain admission → execution pool,
//! with a timer queue for cooldowns and retry backoff.
//!
//! Workers only ever run admitted attempts. A mailbox whose rate domain is
//! cooling down, or whose retry backoff has not elapsed, is parked in the
//! timer queue; one waiting for a concurrency slot is parked until a running
//! attempt ends. Either way it holds no worker, so other tenants keep
//! running. Ready mailboxes are served round-robin across tenants, so one
//! customer with many queued mailboxes cannot starve another.

use super::batch_work_item::{AttemptOutcome, BatchAttemptRunner, MailboxTask};
use super::rate_domains::{Admission, Blocked, RateDomainLimiter};
use crate::bulk_import::BulkJob;
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, VecDeque},
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

/// Longest the scheduler sleeps before re-checking cancellation.
const SCHEDULER_TICK: Duration = Duration::from_millis(100);

/// Round-robin queues keyed by tenant. Each pop serves the next tenant's
/// oldest item and rotates that tenant to the back.
pub(crate) struct FairQueue<T> {
    order: VecDeque<String>,
    queues: HashMap<String, VecDeque<T>>,
    len: usize,
}

impl<T> FairQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            order: VecDeque::new(),
            queues: HashMap::new(),
            len: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Queue `item` behind (or, with `front`, ahead of) its tenant's others.
    pub(crate) fn push(&mut self, tenant: &str, item: T, front: bool) {
        let queue = self.queues.entry(tenant.to_owned()).or_insert_with(|| {
            self.order.push_back(tenant.to_owned());
            VecDeque::new()
        });
        if front {
            queue.push_front(item);
        } else {
            queue.push_back(item);
        }
        self.len += 1;
    }

    pub(crate) fn pop_next(&mut self) -> Option<T> {
        let tenant = self.order.pop_front()?;
        let queue = self.queues.get_mut(&tenant)?;
        let item = queue.pop_front();
        if queue.is_empty() {
            self.queues.remove(&tenant);
        } else {
            self.order.push_back(tenant);
        }
        if item.is_some() {
            self.len -= 1;
        }
        item
    }

    pub(crate) fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        self.order.clear();
        self.len = 0;
        self.queues.drain().flat_map(|(_, queue)| queue)
    }
}

/// Tasks parked until an instant, earliest first.
struct TimerQueue {
    heap: BinaryHeap<Reverse<(Instant, u64)>>,
    tasks: HashMap<u64, MailboxTask>,
    sequence: u64,
}

impl TimerQueue {
    fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            tasks: HashMap::new(),
            sequence: 0,
        }
    }

    fn len(&self) -> usize {
        self.tasks.len()
    }

    fn park(&mut self, until: Instant, task: MailboxTask) {
        self.sequence += 1;
        self.heap.push(Reverse((until, self.sequence)));
        self.tasks.insert(self.sequence, task);
    }

    fn next_due(&self) -> Option<Instant> {
        self.heap.peek().map(|Reverse((until, _))| *until)
    }

    fn pop_due(&mut self, now: Instant) -> Option<MailboxTask> {
        let Reverse((until, sequence)) = *self.heap.peek()?;
        if until > now {
            return None;
        }
        self.heap.pop();
        self.tasks.remove(&sequence)
    }

    fn drain(&mut self) -> impl Iterator<Item = MailboxTask> + '_ {
        self.heap.clear();
        self.tasks.drain().map(|(_, task)| task)
    }
}

/// What the scheduler needs from the attempt executor.
pub(crate) trait AttemptExecutor: Send + Sync + 'static {
    fn cancelled(&self) -> bool;
    fn mark_failed(&self);
    fn rate_limiter(&self) -> &Arc<RateDomainLimiter>;
    /// Run one admitted attempt; the admission must be released on return.
    fn run_attempt(&self, task: MailboxTask, admission: Admission) -> AttemptOutcome;
    fn cancel_waiting(&self, task: MailboxTask);
    fn fail_unschedulable(&self, task: MailboxTask, reason: &str);
}

impl AttemptExecutor for BatchAttemptRunner {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn mark_failed(&self) {
        self.failed.store(true, Ordering::Relaxed);
    }

    fn rate_limiter(&self) -> &Arc<RateDomainLimiter> {
        &self.provider_limiter
    }

    fn run_attempt(&self, task: MailboxTask, admission: Admission) -> AttemptOutcome {
        BatchAttemptRunner::run_attempt(self, task, admission)
    }

    fn cancel_waiting(&self, task: MailboxTask) {
        BatchAttemptRunner::cancel_waiting(self, task);
    }

    fn fail_unschedulable(&self, task: MailboxTask, reason: &str) {
        BatchAttemptRunner::fail_unschedulable(self, task, reason);
    }
}

enum WorkerReport {
    Attempt(AttemptOutcome),
    Panicked,
}

/// Resolves a page of admitted job IDs to executable jobs, in order. A job
/// whose plan or credentials cannot be prepared is an `Err` for that job
/// alone; an `Err` for the whole page means the source itself failed.
pub(crate) type JobLoader =
    Box<dyn FnMut(&[String]) -> Result<Vec<Result<BulkJob, String>>, String> + Send>;

/// Jobs to load per page. Small pages keep only a bounded number of
/// executable plans (and loaded credentials) in memory ahead of the workers.
const LOAD_PAGE: usize = 64;

/// Unscheduled jobs, read from their source only as the queues need them.
pub(crate) struct JobSource {
    pub(crate) job_ids: Vec<String>,
    pub(crate) child_run_ids: Vec<String>,
    pub(crate) checkpoints: Vec<Option<String>>,
    pub(crate) loader: JobLoader,
    next: usize,
    page: std::collections::VecDeque<(usize, Result<BulkJob, String>)>,
}

impl JobSource {
    pub(crate) fn new(
        job_ids: Vec<String>,
        child_run_ids: Vec<String>,
        checkpoints: Vec<Option<String>>,
        loader: JobLoader,
    ) -> Self {
        Self {
            job_ids,
            child_run_ids,
            checkpoints,
            loader,
            next: 0,
            page: std::collections::VecDeque::new(),
        }
    }

    /// A source over jobs already in memory.
    #[cfg(test)]
    pub(crate) fn from_jobs(
        jobs: Vec<BulkJob>,
        job_ids: Vec<String>,
        child_run_ids: Vec<String>,
        checkpoints: Vec<Option<String>>,
    ) -> Self {
        let mut jobs = jobs.into_iter();
        Self::new(
            job_ids,
            child_run_ids,
            checkpoints,
            Box::new(move |ids| {
                Ok(ids
                    .iter()
                    .map(|_| jobs.next().ok_or_else(|| "missing job".to_owned()))
                    .collect())
            }),
        )
    }

    /// The next task, or `Err(())` if the source failed or a job has no
    /// durable identity.
    fn next_task(&mut self) -> Option<Result<MailboxTask, ()>> {
        if self.page.is_empty() {
            if self.next >= self.job_ids.len() {
                return None;
            }
            let end = (self.next + LOAD_PAGE).min(self.job_ids.len());
            let loaded = match (self.loader)(&self.job_ids[self.next..end]) {
                Ok(loaded) if loaded.len() == end - self.next => loaded,
                _ => {
                    self.next = self.job_ids.len();
                    return Some(Err(()));
                }
            };
            self.page = (self.next..end).zip(loaded).collect();
            self.next = end;
        }
        let (index, job) = self.page.pop_front()?;
        let (Some(job_id), Some(child_run_id)) = (
            self.job_ids.get(index).cloned(),
            self.child_run_ids.get(index).cloned(),
        ) else {
            return Some(Err(()));
        };
        let checkpoint = self.checkpoints.get(index).cloned().unwrap_or_default();
        Some(Ok(match job {
            Ok(job) => MailboxTask::new(index, job_id, child_run_id, checkpoint, &job),
            Err(error) => MailboxTask::unpreparable(index, job_id, child_run_id, error),
        }))
    }
}

pub(crate) struct ScheduleResult {
    pub(crate) worker_panicked: bool,
    pub(crate) enqueue_failed: bool,
}

/// Run every job of one batch to a terminal state (or to cancellation)
/// using `concurrency` execution workers.
pub(crate) fn run_batch_schedule<R: AttemptExecutor>(
    runner: Arc<R>,
    concurrency: usize,
    mut source: JobSource,
) -> ScheduleResult {
    let concurrency = concurrency.max(1);
    let (dispatch_tx, dispatch_rx) = crossbeam_channel::unbounded::<(MailboxTask, Admission)>();
    let (report_tx, report_rx) = crossbeam_channel::unbounded::<WorkerReport>();
    let workers = (0..concurrency)
        .map(|_| {
            let dispatch_rx = dispatch_rx.clone();
            let report_tx = report_tx.clone();
            let runner = Arc::clone(&runner);
            thread::spawn(move || {
                while let Ok((task, admission)) = dispatch_rx.recv() {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        runner.run_attempt(task, admission)
                    }));
                    let report = match outcome {
                        Ok(outcome) => WorkerReport::Attempt(outcome),
                        Err(_) => WorkerReport::Panicked,
                    };
                    let panicked = matches!(report, WorkerReport::Panicked);
                    if report_tx.send(report).is_err() || panicked {
                        return;
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    drop((dispatch_rx, report_tx));

    // Materialize only a bounded window of not-yet-admitted jobs; parked
    // tasks beyond that window wait in the source instead of in memory.
    let lookahead = concurrency.saturating_mul(2).max(1);
    let max_parked = concurrency.saturating_mul(64).max(256);
    let mut ready = FairQueue::<MailboxTask>::new();
    let mut timers = TimerQueue::new();
    let mut slot_waiting = Vec::<MailboxTask>::new();
    let mut source_open = true;
    let mut alive = concurrency;
    let mut busy = 0_usize;
    let mut result = ScheduleResult {
        worker_panicked: false,
        enqueue_failed: false,
    };
    let requeue = |ready: &mut FairQueue<MailboxTask>, task: MailboxTask, front: bool| {
        let tenant = task.rate_path.fairness_key().to_owned();
        ready.push(&tenant, task, front);
    };

    loop {
        let now = Instant::now();
        if runner.cancelled() {
            for task in ready
                .drain()
                .chain(timers.drain())
                .chain(slot_waiting.drain(..))
            {
                runner.cancel_waiting(task);
            }
            while source_open {
                match source.next_task() {
                    Some(Ok(task)) => runner.cancel_waiting(task),
                    Some(Err(())) => {
                        runner.mark_failed();
                        result.enqueue_failed = true;
                        source_open = false;
                    }
                    None => source_open = false,
                }
            }
        } else {
            while let Some(task) = timers.pop_due(now) {
                requeue(&mut ready, task, false);
            }
            while source_open
                && ready.len() < lookahead
                && ready.len() + timers.len() + slot_waiting.len() < max_parked
            {
                match source.next_task() {
                    Some(Ok(task)) => requeue(&mut ready, task, false),
                    Some(Err(())) => {
                        runner.mark_failed();
                        result.enqueue_failed = true;
                        source_open = false;
                    }
                    None => source_open = false,
                }
            }
            while busy < alive {
                let Some(task) = ready.pop_next() else {
                    break;
                };
                match runner.rate_limiter().try_admit(&task.rate_path) {
                    Ok(admission) => {
                        if dispatch_tx.send((task, admission)).is_err() {
                            alive = 0;
                            break;
                        }
                        busy += 1;
                    }
                    Err(Blocked::Until(until)) => timers.park(until, task),
                    // Every slot is held by a running attempt, so `busy > 0`
                    // and its completion will requeue this task.
                    Err(Blocked::Slot) if busy > 0 => slot_waiting.push(task),
                    Err(Blocked::Slot) => runner.fail_unschedulable(
                        task,
                        "rate-domain concurrency accounting is inconsistent; job not started",
                    ),
                    Err(Blocked::Unavailable) => runner.fail_unschedulable(
                        task,
                        "rate-domain limiter is unavailable; job not started",
                    ),
                }
            }
        }

        let drained = !source_open && ready.is_empty() && timers.len() == 0;
        if busy == 0 && (alive == 0 || (drained && slot_waiting.is_empty())) {
            break;
        }

        let timeout = timers
            .next_due()
            .map_or(SCHEDULER_TICK, |due| due.saturating_duration_since(now))
            .min(SCHEDULER_TICK);
        let report = match report_rx.recv_timeout(timeout) {
            Ok(report) => report,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        busy = busy.saturating_sub(1);
        match report {
            WorkerReport::Attempt(AttemptOutcome::Finished) => {}
            WorkerReport::Attempt(AttemptOutcome::Retry { task, delay }) => {
                if runner.cancelled() {
                    runner.cancel_waiting(*task);
                } else {
                    timers.park(Instant::now() + delay, *task);
                }
            }
            WorkerReport::Panicked => {
                alive = alive.saturating_sub(1);
                result.worker_panicked = true;
                runner.mark_failed();
            }
        }
        // A finished attempt released its slots; let slot-limited tasks
        // compete again, ahead of their tenants' newer work.
        for task in slot_waiting.drain(..).rev() {
            requeue(&mut ready, task, true);
        }
    }
    drop(dispatch_tx);
    for worker in workers {
        if worker.join().is_err() {
            result.worker_panicked = true;
            runner.mark_failed();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::FairQueue;

    /// Real batch runs against a fake imapsync, driven by a test reducer.
    #[cfg(unix)]
    mod composed {
        use crate::Event;
        use crate::bulk_import::BulkJob;
        use crate::controller::batch::BatchExecutionMode;
        use crate::controller::batch_worker::{
            BatchExecutionContext, BatchLimiters, rate_domain_path, spawn_batch_worker_with,
        };
        use crate::controller::rate_domains::{DomainLevel, RateDomainLimiter, Side};
        use crate::process::ProcessLaunchLimiter;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, atomic::AtomicBool, mpsc};
        use std::time::{Duration, Instant};

        pub(super) struct Run {
            /// (job id, when its engine process start was reported)
            pub(super) starts: Vec<(String, Instant)>,
            pub(super) finished: Vec<(String, String)>,
            pub(super) began: Instant,
        }

        pub(super) fn form(engine: &std::path::Path, user: &str) -> crate::Form {
            let mut form = crate::Form::default();
            form.profile.engine = crate::core::Engine::ImapSync;
            form.profile.imapsync_path = engine.to_string_lossy().into_owned();
            form.profile.source_host = "old.example".into();
            form.profile.destination_host = "new.example".into();
            form.profile.source_user = user.into();
            form.profile.destination_user = user.into();
            form.source_password = String::from("source-secret").into();
            form.destination_password = String::from("destination-secret").into();
            form.dry_run = true;
            form
        }

        /// Write a fake engine. Users starting with `fail` fail their first
        /// attempt with a transient disconnect, then succeed.
        pub(super) fn engine(directory: &std::path::Path) -> std::path::PathBuf {
            let engine = directory.join("fake-imapsync");
            let script = format!(
                "#!/bin/sh\nuser=\"\"\nwhile [ $# -gt 0 ]; do [ \"$1\" = \"--user1\" ] && user=\"$2\"; shift; done\n\
                 case \"$user\" in fail*) marker=\"{dir}/$user.tried\"; if [ ! -e \"$marker\" ]; then : > \"$marker\"; echo 'connection reset by peer' >&2; exit 1; fi;; esac\nexit 0\n",
                dir = directory.display()
            );
            std::fs::write(&engine, script).unwrap();
            std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700)).unwrap();
            engine
        }

        pub(super) fn temp_directory() -> std::path::PathBuf {
            let directory = std::env::temp_dir()
                .join(format!("mailswiftsync-schedule-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            directory
        }

        pub(super) fn run(
            forms: Vec<crate::Form>,
            concurrency: usize,
            retry_count: usize,
            limiters: BatchLimiters,
        ) -> Run {
            let count = forms.len();
            let jobs = forms
                .into_iter()
                .enumerate()
                .map(|(index, form)| {
                    BulkJob::from_form(format!("job {index}"), form, "Ready".into())
                })
                .collect::<Vec<_>>();
            let (tx, rx) = mpsc::sync_channel(256);
            let began = Instant::now();
            let coordinator = spawn_batch_worker_with(
                BatchExecutionContext {
                    concurrency,
                    mode: BatchExecutionMode::Preflight,
                    retry_count,
                    job_count: count,
                    queue_job_ids: (0..count).map(|index| format!("job-{index}")).collect(),
                    child_run_ids: (0..count).map(|index| format!("run-{index}")).collect(),
                    queue_checkpoints: vec![None; count],
                    batch_project_id: "project".into(),
                    batch_run_id: "batch".into(),
                    imapsync_executables: {
                        let mut executables = jobs
                            .iter()
                            .map(|job: &BulkJob| job.profile().imapsync_path)
                            .collect::<Vec<_>>();
                        executables.dedup();
                        executables
                    },
                    loader: {
                        let mut jobs = jobs.into_iter();
                        Box::new(move |ids: &[String]| {
                            Ok(ids
                                .iter()
                                .map(|_| jobs.next().ok_or_else(|| "missing job".to_owned()))
                                .collect())
                        })
                    },
                    verification_state_path: None,
                    diagnostic_logger: None,
                },
                tx,
                Arc::new(AtomicBool::new(false)),
                limiters,
            );
            let mut starts = Vec::new();
            let mut finished = Vec::new();
            while let Ok(event) = rx.recv_timeout(Duration::from_secs(60)) {
                match event {
                    Event::ProcessStarted(_, job_id, _, _, _, _, _, reply) => {
                        starts.push((job_id, Instant::now()));
                        let _ = reply.send(Ok(()));
                    }
                    Event::ClaimBatch { reply, .. }
                    | Event::EngineVersion { reply, .. }
                    | Event::TransferAttempt { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    Event::JobFinished {
                        reply,
                        job_id,
                        state,
                        ..
                    } => {
                        finished.push((job_id, state));
                        let _ = reply.send(Ok(()));
                    }
                    Event::Finished(_) => break,
                    _ => {}
                }
            }
            coordinator.join().unwrap();
            Run {
                starts,
                finished,
                began,
            }
        }

        pub(super) fn limiters(concurrency: usize, starts_per_second: usize) -> BatchLimiters {
            BatchLimiters {
                launch: Arc::new(ProcessLaunchLimiter::new(starts_per_second)),
                rate: Arc::new(RateDomainLimiter::new(concurrency)),
            }
        }

        pub(super) fn source_domain(
            form: &crate::Form,
            level: DomainLevel,
        ) -> crate::controller::rate_domains::DomainKey {
            rate_domain_path(form).domain(Side::Source, level).clone()
        }
    }

    /// Workers held behind one provider cooldown and released together must
    /// still start engine processes no faster than the launch rate: the
    /// start token is taken immediately before spawn.
    #[cfg(unix)]
    #[test]
    fn released_provider_cooldown_does_not_burst_process_starts() {
        use crate::controller::rate_domains::DomainLevel;
        const WORKERS: usize = 4;
        const STARTS_PER_SECOND: usize = 10;
        let directory = composed::temp_directory();
        let engine = composed::engine(&directory);
        let forms = (0..WORKERS)
            .map(|index| composed::form(&engine, &format!("user{index}@a.example")))
            .collect::<Vec<_>>();
        let limiters = composed::limiters(WORKERS, STARTS_PER_SECOND);
        // Longer than the time the workers need to drain any start tokens
        // they could bank while waiting for admission.
        limiters.rate.hold_until(
            &composed::source_domain(&forms[0], DomainLevel::Provider),
            std::time::Instant::now() + std::time::Duration::from_millis(800),
        );
        let run = composed::run(forms, WORKERS, 0, limiters);
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(run.starts.len(), WORKERS, "{:?}", run.finished);
        let spacing = std::time::Duration::from_secs_f64(1.0 / STARTS_PER_SECOND as f64);
        for pair in run.starts.windows(2) {
            let gap = pair[1].1.duration_since(pair[0].1);
            // Allow scheduler jitter, never a burst.
            assert!(
                gap >= spacing.mul_f64(0.7),
                "process starts {gap:?} apart exceed {STARTS_PER_SECOND}/s"
            );
        }
    }

    /// A tenant in cooldown is parked, not held by workers: with every
    /// queued job ahead of it belonging to the paused tenant, another
    /// tenant's jobs still start immediately.
    #[cfg(unix)]
    #[test]
    fn paused_tenant_does_not_block_other_tenants() {
        use crate::controller::rate_domains::DomainLevel;
        let directory = composed::temp_directory();
        let engine = composed::engine(&directory);
        let mut forms = (0..4)
            .map(|index| composed::form(&engine, &format!("a{index}@paused.example")))
            .collect::<Vec<_>>();
        forms
            .extend((0..2).map(|index| composed::form(&engine, &format!("b{index}@open.example"))));
        let limiters = composed::limiters(2, 50);
        let pause = std::time::Duration::from_millis(1_500);
        limiters.rate.hold_until(
            &composed::source_domain(&forms[0], DomainLevel::Tenant),
            std::time::Instant::now() + pause,
        );
        let run = composed::run(forms, 2, 0, limiters);
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(run.starts.len(), 6, "{:?}", run.finished);
        let started = |prefix: &str| {
            run.starts
                .iter()
                .filter(|(job, _)| {
                    let index: usize = job.trim_start_matches("job-").parse().unwrap();
                    (prefix == "a") == (index < 4)
                })
                .map(|(_, at)| at.duration_since(run.began))
                .collect::<Vec<_>>()
        };
        for at in started("b") {
            assert!(
                at < pause / 2,
                "open tenant waited {at:?} behind the paused one"
            );
        }
        for at in started("a") {
            assert!(at >= pause, "paused tenant started at {at:?}");
        }
    }

    /// Fails each job's first attempt with a fixed backoff, then succeeds,
    /// recording the order in which attempts start.
    struct BackoffExecutor {
        limiter: std::sync::Arc<crate::controller::rate_domains::RateDomainLimiter>,
        retried: std::sync::Mutex<std::collections::HashSet<usize>>,
        started: std::sync::Mutex<Vec<(usize, std::time::Instant)>>,
        backoff: std::time::Duration,
    }

    impl super::AttemptExecutor for BackoffExecutor {
        fn cancelled(&self) -> bool {
            false
        }

        fn mark_failed(&self) {}

        fn rate_limiter(
            &self,
        ) -> &std::sync::Arc<crate::controller::rate_domains::RateDomainLimiter> {
            &self.limiter
        }

        fn run_attempt(
            &self,
            task: crate::controller::batch_work_item::MailboxTask,
            admission: crate::controller::rate_domains::Admission,
        ) -> crate::controller::batch_work_item::AttemptOutcome {
            use crate::controller::batch_work_item::AttemptOutcome;
            self.started
                .lock()
                .unwrap()
                .push((task.index, std::time::Instant::now()));
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(admission);
            if self.retried.lock().unwrap().insert(task.index) {
                AttemptOutcome::Retry {
                    task: Box::new(task),
                    delay: self.backoff,
                }
            } else {
                AttemptOutcome::Finished
            }
        }

        fn cancel_waiting(&self, _: crate::controller::batch_work_item::MailboxTask) {
            panic!("nothing is cancelled");
        }

        fn fail_unschedulable(
            &self,
            _: crate::controller::batch_work_item::MailboxTask,
            reason: &str,
        ) {
            panic!("{reason}");
        }
    }

    /// A job waiting out its retry backoff is parked in the timer queue and
    /// frees its worker: with one worker, every other job runs during the
    /// delay instead of after it.
    #[test]
    fn retry_backoff_does_not_occupy_a_worker() {
        let backoff = std::time::Duration::from_millis(600);
        let executor = std::sync::Arc::new(BackoffExecutor {
            limiter: std::sync::Arc::new(crate::controller::rate_domains::RateDomainLimiter::new(
                1,
            )),
            retried: Default::default(),
            started: Default::default(),
            backoff,
        });
        let jobs = (0..3)
            .map(|index| {
                let mut form = crate::Form::default();
                form.profile.source_host = "old.example".into();
                form.profile.source_user = format!("user{index}@t{index}.example");
                crate::bulk_import::BulkJob::from_form(format!("job {index}"), form, "Ready".into())
            })
            .collect::<Vec<_>>();
        let began = std::time::Instant::now();
        let result = super::run_batch_schedule(
            std::sync::Arc::clone(&executor),
            1,
            super::JobSource::from_jobs(
                jobs,
                (0..3).map(|index| format!("job-{index}")).collect(),
                (0..3).map(|index| format!("run-{index}")).collect(),
                vec![None; 3],
            ),
        );
        assert!(!result.worker_panicked && !result.enqueue_failed);
        let started = executor.started.lock().unwrap().clone();
        let order = started.iter().map(|(index, _)| *index).collect::<Vec<_>>();
        // Every first attempt runs before any retry: backoffs overlap
        // instead of serializing behind the single worker.
        assert_eq!(&order[..3], [0, 1, 2], "{order:?}");
        assert_eq!(order.len(), 6, "{order:?}");
        let first_retry = started[3].1.duration_since(began);
        assert!(first_retry >= backoff, "retried after {first_retry:?}");
        let total = began.elapsed();
        assert!(
            total < backoff * 2,
            "three overlapping backoffs took {total:?}; they were serialized"
        );
    }

    /// Cancels the batch during its first attempt, which asks for a long
    /// backoff; counts the tasks the scheduler settles as cancelled.
    struct CancellingExecutor {
        limiter: std::sync::Arc<crate::controller::rate_domains::RateDomainLimiter>,
        cancel: std::sync::atomic::AtomicBool,
        attempts: std::sync::atomic::AtomicUsize,
        cancelled: std::sync::Mutex<Vec<usize>>,
    }

    impl super::AttemptExecutor for CancellingExecutor {
        fn cancelled(&self) -> bool {
            self.cancel.load(std::sync::atomic::Ordering::Relaxed)
        }

        fn mark_failed(&self) {}

        fn rate_limiter(
            &self,
        ) -> &std::sync::Arc<crate::controller::rate_domains::RateDomainLimiter> {
            &self.limiter
        }

        fn run_attempt(
            &self,
            task: crate::controller::batch_work_item::MailboxTask,
            admission: crate::controller::rate_domains::Admission,
        ) -> crate::controller::batch_work_item::AttemptOutcome {
            drop(admission);
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            crate::controller::batch_work_item::AttemptOutcome::Retry {
                task: Box::new(task),
                delay: std::time::Duration::from_secs(60),
            }
        }

        fn cancel_waiting(&self, task: crate::controller::batch_work_item::MailboxTask) {
            self.cancelled.lock().unwrap().push(task.index);
        }

        fn fail_unschedulable(
            &self,
            _: crate::controller::batch_work_item::MailboxTask,
            reason: &str,
        ) {
            panic!("{reason}");
        }
    }

    /// Cancellation settles the retrying task, queued tasks, and jobs never
    /// materialized from the source, without waiting out the backoff.
    #[test]
    fn cancellation_settles_parked_queued_and_unstarted_jobs() {
        let executor = std::sync::Arc::new(CancellingExecutor {
            limiter: std::sync::Arc::new(crate::controller::rate_domains::RateDomainLimiter::new(
                1,
            )),
            cancel: Default::default(),
            attempts: Default::default(),
            cancelled: Default::default(),
        });
        let count = 10;
        let jobs = (0..count)
            .map(|index| {
                let mut form = crate::Form::default();
                form.profile.source_user = format!("user{index}@t.example");
                crate::bulk_import::BulkJob::from_form(format!("job {index}"), form, "Ready".into())
            })
            .collect::<Vec<_>>();
        let began = std::time::Instant::now();
        super::run_batch_schedule(
            std::sync::Arc::clone(&executor),
            1,
            super::JobSource::from_jobs(
                jobs,
                (0..count).map(|index| format!("job-{index}")).collect(),
                (0..count).map(|index| format!("run-{index}")).collect(),
                vec![None; count],
            ),
        );
        assert!(began.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(
            executor.attempts.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        let mut cancelled = executor.cancelled.lock().unwrap().clone();
        cancelled.sort_unstable();
        assert_eq!(cancelled, (0..count).collect::<Vec<_>>());
    }

    #[test]
    fn fair_queue_serves_tenants_round_robin() {
        let mut queue = FairQueue::new();
        for item in ["a1", "a2", "a3"] {
            queue.push("a", item, false);
        }
        queue.push("b", "b1", false);
        queue.push("c", "c1", false);
        let order = std::iter::from_fn(|| queue.pop_next()).collect::<Vec<_>>();
        assert_eq!(order, ["a1", "b1", "c1", "a2", "a3"]);
        assert!(queue.is_empty());
    }

    #[test]
    fn fair_queue_front_push_preserves_a_tenants_order() {
        let mut queue = FairQueue::new();
        queue.push("a", "a3", false);
        queue.push("a", "a2", true);
        queue.push("a", "a1", true);
        assert_eq!(queue.len(), 3);
        let order = std::iter::from_fn(|| queue.pop_next()).collect::<Vec<_>>();
        assert_eq!(order, ["a1", "a2", "a3"]);
    }
}
