use super::batch::BatchExecutionMode;
use super::batch_work_item::{BatchWorkerContext, process_batch_work_items};
use crate::{Event, StreamOutcome, bulk_import::BulkJob, process::ProcessLaunchLimiter};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

const BATCH_PROCESS_STARTS_PER_SECOND: usize = 2;
const MAX_BATCH_PENDING_EVENTS: usize = 4_096;

type BatchWorkItem = (usize, String, String, Option<String>, BulkJob);

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
    } = context;
    let launch_limiter = Arc::new(ProcessLaunchLimiter::new(BATCH_PROCESS_STARTS_PER_SECOND));
    thread::spawn(move || {
        let failed = Arc::new(AtomicBool::new(false));
        let terminal_jobs = Arc::new(Mutex::new(HashSet::new()));
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
            let batch_project_id = batch_project_id.clone();
            let batch_run_id = batch_run_id.clone();
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
                    batch_project_id,
                    batch_run_id,
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
                let _ = tx.send(Event::JobFinished {
                    job_id,
                    child_run_id,
                    state: "attention".into(),
                    detail: "worker stopped unexpectedly".into(),
                    credential_fingerprint: None,
                });
            }
            let _ = tx.send(Event::Line(
                    "A batch worker stopped unexpectedly; unresolved jobs require review before retrying."
                        .into(),
                ));
        }
        let _ = tx.send(Event::Finished(if cancel.load(Ordering::Relaxed) {
            Err("batch cancelled".into())
        } else if failed.load(Ordering::Relaxed) {
            Err("one or more batch jobs failed".into())
        } else {
            Ok(StreamOutcome::Completed)
        }));
    });
}
