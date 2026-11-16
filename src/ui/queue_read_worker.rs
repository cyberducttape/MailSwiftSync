//! Background SQLite reads for the virtualized mailbox queue.
//!
//! Filtering, viewport paging, row loading, and the selection projection all
//! share one read-only worker connection. Requests carry the complete view
//! identity; responses for an old filter or queue generation are discarded.

use crate::core::StateStore;
use crate::ui::queue_model::{MailboxQueue, QueueReadProjection, QueueReadRequest};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

struct Response {
    request: QueueReadRequest,
    result: Result<QueueReadProjection, String>,
}

pub(crate) enum QueueReadPoll {
    Idle,
    Applied,
    Failed(String),
}

pub(crate) struct QueueReadWorker {
    requests: Sender<QueueReadRequest>,
    responses: Receiver<Response>,
    in_flight: Option<QueueReadRequest>,
    last_sent: Option<QueueReadRequest>,
    desired: Option<QueueReadRequest>,
    retry_after: Option<std::time::Instant>,
}

fn worker_loop(path: PathBuf, requests: Receiver<QueueReadRequest>, responses: Sender<Response>) {
    let store = match StateStore::open_readonly(&path) {
        Ok(store) => store,
        Err(error) => {
            let _ = responses.send(Response {
                request: QueueReadRequest {
                    project_id: None,
                    len: 0,
                    generation: 0,
                    search: String::new(),
                    state_filter: String::new(),
                    page_matches: false,
                    page_range: 0..0,
                    load_rowids: Vec::new(),
                    transient: Default::default(),
                    selected_ids: Vec::new(),
                    all_selected: false,
                    selection_summary_requested: false,
                },
                result: Err(format!(
                    "Could not open mailbox queue read connection: {error}"
                )),
            });
            return;
        }
    };
    let mut queue = MailboxQueue::default();
    while let Ok(mut request) = requests.recv() {
        while let Ok(newer) = requests.try_recv() {
            request = newer;
        }
        let result = Ok(queue.read_projection(&store, request.clone()));
        if responses.send(Response { request, result }).is_err() {
            return;
        }
    }
}

impl QueueReadWorker {
    pub(crate) fn spawn(path: PathBuf) -> std::io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        thread::Builder::new()
            .name("mailswiftsync-mailbox-queue-read".into())
            .spawn(move || worker_loop(path, request_rx, response_tx))?;
        Ok(Self {
            requests: request_tx,
            responses: response_rx,
            in_flight: None,
            last_sent: None,
            desired: None,
            retry_after: None,
        })
    }

    pub(crate) fn request(&mut self, request: QueueReadRequest) {
        self.desired = Some(request);
        self.send_if_idle();
    }

    fn send_if_idle(&mut self) -> bool {
        if self.in_flight.is_some() {
            return true;
        }
        let Some(request) = self.desired.clone() else {
            return true;
        };
        if self
            .retry_after
            .is_some_and(|at| std::time::Instant::now() < at)
        {
            return true;
        }
        if self.last_sent.as_ref() == Some(&request) {
            return true;
        }
        if self.requests.send(request.clone()).is_err() {
            self.last_sent = Some(request);
            return false;
        }
        self.in_flight = Some(request.clone());
        self.last_sent = Some(request);
        self.retry_after = None;
        true
    }

    pub(crate) fn poll(
        &mut self,
        queue: &mut MailboxQueue,
        current: &QueueReadRequest,
    ) -> QueueReadPoll {
        self.desired = Some(current.clone());
        let mut outcome = QueueReadPoll::Idle;
        match self.responses.try_recv() {
            Ok(response) => {
                self.in_flight = None;
                match response.result {
                    Ok(projection)
                        if same_view(&response.request, current)
                            && projection.project_id == current.project_id
                            && projection.generation == current.generation =>
                    {
                        if let Some(error) = projection.filter_error.as_ref() {
                            self.last_sent = None;
                            self.retry_after = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(500),
                            );
                            outcome = QueueReadPoll::Failed(error.clone());
                        } else {
                            if projection
                                .selection_summary
                                .as_ref()
                                .is_some_and(Result::is_err)
                            {
                                self.last_sent = None;
                                self.retry_after = Some(
                                    std::time::Instant::now()
                                        + std::time::Duration::from_millis(500),
                                );
                            }
                            queue.apply_read_projection(projection);
                            self.retry_after = None;
                            outcome = QueueReadPoll::Applied;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        self.last_sent = None;
                        self.retry_after =
                            Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
                        outcome = QueueReadPoll::Failed(error);
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                return QueueReadPoll::Failed(
                    "mailbox queue read worker terminated unexpectedly".to_owned(),
                );
            }
            Err(TryRecvError::Empty) => {}
        }
        if !self.send_if_idle() {
            return QueueReadPoll::Failed(
                "mailbox queue read worker terminated unexpectedly".to_owned(),
            );
        }
        outcome
    }

    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight.is_some()
            || self.retry_after.is_some()
            || self.desired.as_ref().is_some_and(|desired| {
                self.last_sent.as_ref() != Some(desired)
                    && self
                        .retry_after
                        .is_none_or(|at| std::time::Instant::now() >= at)
            })
    }
}

fn same_view(left: &QueueReadRequest, right: &QueueReadRequest) -> bool {
    left.project_id == right.project_id
        && left.len == right.len
        && left.generation == right.generation
        && left.search == right.search
        && left.state_filter == right.state_filter
        && left.page_matches == right.page_matches
        && left.transient == right.transient
        && left.selected_ids == right.selected_ids
        && left.all_selected == right.all_selected
        && left.selection_summary_requested == right.selection_summary_requested
}

#[cfg(test)]
mod tests {
    use super::same_view;
    use crate::ui::queue_model::QueueReadRequest;

    fn request() -> QueueReadRequest {
        QueueReadRequest {
            project_id: Some("project".into()),
            len: 20,
            generation: 4,
            search: "alice".into(),
            state_filter: "queued".into(),
            page_matches: false,
            page_range: 0..32,
            load_rowids: vec![1, 2],
            transient: Default::default(),
            selected_ids: vec!["mailbox-1".into()],
            all_selected: false,
            selection_summary_requested: true,
        }
    }

    #[test]
    fn view_identity_ignores_viewport_but_rejects_stale_generations_and_filters() {
        let original = request();
        let mut next_page = original.clone();
        next_page.page_range = 32..64;
        next_page.load_rowids = vec![3, 4];
        assert!(same_view(&original, &next_page));

        let mut stale_generation = original.clone();
        stale_generation.generation += 1;
        assert!(!same_view(&original, &stale_generation));

        let mut changed_filter = original.clone();
        changed_filter.search = "bob".into();
        assert!(!same_view(&original, &changed_filter));

        let mut changed_selection = original.clone();
        changed_selection.selected_ids = vec!["mailbox-2".into()];
        assert!(!same_view(&original, &changed_selection));
    }
}
