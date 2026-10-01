//! Off-thread refresh of the workspace read model.
//!
//! `WorkspaceSnapshot::refresh` performs several SQLite reads and, under
//! contention, each can wait up to the connection's `busy_timeout`. Running it
//! from the egui update path could freeze a frame for seconds. The GUI instead
//! hands refresh requests to one read-model worker that owns a separate
//! read-only connection and its own snapshot. Each request carries a
//! generation; at most one is in flight, and a completed snapshot replaces the
//! UI copy only when it was built for the view options still on screen.
//! Headless and test callers keep the synchronous path.

use crate::core::StateStore;
use crate::ui::workspace::{OwnedRefreshOptions, REFRESH_INTERVAL, WorkspaceSnapshot};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Instant;

struct SnapshotRequest {
    generation: u64,
    invalidate: bool,
    options: OwnedRefreshOptions,
}

enum SnapshotResponse {
    /// `snapshot` is `None` when the refresh found nothing new.
    Refreshed {
        generation: u64,
        snapshot: Option<Box<WorkspaceSnapshot>>,
    },
    /// The worker could not open its read-only connection.
    Unavailable(String),
}

pub(crate) enum SnapshotPoll {
    Idle,
    /// A snapshot matching the current view was installed.
    Applied,
    /// The worker is gone; the caller should fall back to synchronous refresh.
    Failed(String),
}

pub(crate) struct SnapshotWorker {
    requests: Sender<SnapshotRequest>,
    responses: Receiver<SnapshotResponse>,
    next_generation: u64,
    in_flight: Option<(u64, OwnedRefreshOptions)>,
    last_sent: Option<OwnedRefreshOptions>,
    last_request_at: Option<Instant>,
    invalidate_requested: bool,
}

fn worker_loop(
    path: PathBuf,
    requests: Receiver<SnapshotRequest>,
    responses: Sender<SnapshotResponse>,
    simulated_contention: std::time::Duration,
) {
    let store = match StateStore::open_readonly(&path) {
        Ok(store) => store,
        Err(error) => {
            let _ = responses.send(SnapshotResponse::Unavailable(error.to_string()));
            return;
        }
    };
    let mut snapshot = WorkspaceSnapshot::default();
    while let Ok(mut request) = requests.recv() {
        // Coalesce a backlog to its newest request, keeping any invalidation.
        while let Ok(newer) = requests.try_recv() {
            request = SnapshotRequest {
                invalidate: request.invalidate || newer.invalidate,
                ..newer
            };
        }
        if request.invalidate {
            snapshot.invalidate();
        }
        if !simulated_contention.is_zero() {
            thread::sleep(simulated_contention);
        }
        let changed = snapshot.refresh(&store, request.options.borrowed());
        let response = SnapshotResponse::Refreshed {
            generation: request.generation,
            snapshot: changed.then(|| Box::new(snapshot.clone())),
        };
        if responses.send(response).is_err() {
            return;
        }
    }
}

impl SnapshotWorker {
    pub(crate) fn spawn(path: PathBuf) -> std::io::Result<Self> {
        Self::spawn_with_contention(path, std::time::Duration::ZERO)
    }

    /// `simulated_contention` stands in for SQLite `busy_timeout` waits in
    /// tests; production always passes zero.
    fn spawn_with_contention(
        path: PathBuf,
        simulated_contention: std::time::Duration,
    ) -> std::io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        thread::Builder::new()
            .name("mailswiftsync-read-model".into())
            .spawn(move || worker_loop(path, request_rx, response_tx, simulated_contention))?;
        Ok(Self {
            requests: request_tx,
            responses: response_rx,
            next_generation: 0,
            in_flight: None,
            last_sent: None,
            last_request_at: None,
            invalidate_requested: false,
        })
    }

    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    pub(crate) fn request_invalidation(&mut self) {
        self.invalidate_requested = true;
    }

    /// Install a completed snapshot if it matches `current`, then send the
    /// next request when one is due. Never blocks on SQLite.
    pub(crate) fn poll(
        &mut self,
        target: &mut WorkspaceSnapshot,
        current: &OwnedRefreshOptions,
    ) -> SnapshotPoll {
        let mut outcome = SnapshotPoll::Idle;
        match self.responses.try_recv() {
            Ok(SnapshotResponse::Refreshed {
                generation,
                snapshot,
            }) => {
                if let Some((expected, options)) = self.in_flight.take()
                    && expected == generation
                    && options == *current
                    && let Some(snapshot) = snapshot
                {
                    *target = *snapshot;
                    outcome = SnapshotPoll::Applied;
                }
            }
            Ok(SnapshotResponse::Unavailable(error)) => return SnapshotPoll::Failed(error),
            Err(TryRecvError::Disconnected) => {
                return SnapshotPoll::Failed(
                    "read-model worker terminated unexpectedly".to_owned(),
                );
            }
            Err(TryRecvError::Empty) => {}
        }
        if target.project_scope() != current.active_project_id.as_deref() {
            target.reset_project_scope(current.active_project_id.clone());
        }
        if self.in_flight.is_none() {
            let due = self.invalidate_requested
                || self.last_sent.as_ref() != Some(current)
                || self
                    .last_request_at
                    .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL);
            if due {
                self.next_generation = self.next_generation.wrapping_add(1);
                let request = SnapshotRequest {
                    generation: self.next_generation,
                    invalidate: std::mem::take(&mut self.invalidate_requested),
                    options: current.clone(),
                };
                if self.requests.send(request).is_err() {
                    return SnapshotPoll::Failed(
                        "read-model worker terminated unexpectedly".to_owned(),
                    );
                }
                self.in_flight = Some((self.next_generation, current.clone()));
                self.last_sent = Some(current.clone());
                self.last_request_at = Some(Instant::now());
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::{SnapshotPoll, SnapshotWorker};
    use crate::App;
    use std::time::{Duration, Instant};

    /// A private state directory, so bootstrap opens a durable store rather
    /// than falling back to memory (shared temp roots are not private).
    fn temp_state_path() -> std::path::PathBuf {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-snapshot-worker-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        directory.join("state.db")
    }

    fn cleanup(path: &std::path::Path) {
        if let Some(directory) = path.parent() {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    fn poll_until(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(app) {
            assert!(
                Instant::now() < deadline,
                "read-model worker never answered"
            );
            std::thread::sleep(Duration::from_millis(5));
            app.refresh_ui_snapshot();
        }
    }

    #[test]
    fn worker_snapshot_reaches_the_ui_and_tracks_new_projects() {
        let path = temp_state_path();
        let mut app = App::from_state_path(Some(&path));
        app.store
            .create_project("Worker project", "source.example:993", "dest.example:993")
            .unwrap();
        assert!(app.enable_snapshot_worker());
        app.refresh_ui_snapshot_now();
        assert!(
            app.background_work_pending(),
            "an in-flight refresh must keep polling"
        );
        poll_until(&mut app, |app| {
            app.ui_snapshot
                .projects
                .iter()
                .any(|project| project.name == "Worker project")
        });
        drop(app);
        cleanup(&path);
    }

    #[test]
    fn slow_database_reads_do_not_block_the_ui_refresh_path() {
        let path = temp_state_path();
        let mut app = App::from_state_path(Some(&path));
        app.snapshot_worker = Some(
            SnapshotWorker::spawn_with_contention(path.clone(), Duration::from_secs(2)).unwrap(),
        );
        app.refresh_ui_snapshot_now();
        let started = Instant::now();
        for _ in 0..20 {
            app.refresh_ui_snapshot();
        }
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "UI refresh waited on the read model for {:?}",
            started.elapsed()
        );
        assert!(app.background_work_pending());
        assert!(app.ui_snapshot.durable_revision().is_none());
        // Repeated periodic requests keep the slow worker busy; the first
        // completed snapshot must still be installed.
        poll_until(&mut app, |app| app.ui_snapshot.durable_revision().is_some());
        assert!(!app.ui_snapshot.is_stale());
        drop(app);
        cleanup(&path);
    }

    #[test]
    fn stale_response_for_a_previous_view_is_not_installed() {
        let path = temp_state_path();
        let mut app = App::from_state_path(Some(&path));
        let mut worker = SnapshotWorker::spawn(path.clone()).unwrap();
        let mut options = app.snapshot_refresh_options();
        let mut target = crate::ui::WorkspaceSnapshot::default();
        assert!(matches!(
            worker.poll(&mut target, &options),
            SnapshotPoll::Idle
        ));
        assert!(worker.in_flight());
        // The view moves on before the response arrives.
        options.verification_offset = 200;
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.in_flight() || worker.last_sent.as_ref() != Some(&options) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
            if let SnapshotPoll::Applied = worker.poll(&mut target, &options) {
                assert_eq!(worker.last_sent.as_ref(), Some(&options));
            }
        }
        app.ui_snapshot = target;
        drop(worker);
        drop(app);
        cleanup(&path);
    }

    #[test]
    fn vanished_worker_falls_back_to_synchronous_refresh() {
        let path = temp_state_path();
        let mut app = App::from_state_path(Some(&path));
        let missing =
            path.with_file_name(format!("mailswiftsync-missing-{}.db", uuid::Uuid::new_v4()));
        app.snapshot_worker = Some(SnapshotWorker::spawn(missing).unwrap());
        poll_until(&mut app, |app| app.snapshot_worker.is_none());
        app.refresh_ui_snapshot_now();
        assert!(!app.ui_snapshot.is_stale());
        drop(app);
        cleanup(&path);
    }
}
