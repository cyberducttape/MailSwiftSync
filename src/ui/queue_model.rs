//! The Mailboxes view's read model over the durable batch queue.
//!
//! The queue itself lives in SQLite (`core::queue`). The GUI keeps only what
//! a frame needs: the row IDs matching the current filter (the virtual-row
//! index), a bounded cache of rows that have been on screen, and two small
//! overlays that are not durable by design: presentation states of rows a
//! running batch has claimed, and session-only data that must never reach the
//! ledger (plaintext-import passwords and preflight credential fingerprints).

use crate::controller::{BulkQueueSummary, queue::SessionSecrets};
use crate::core::{QueueRow, QueueSelectionSummary, StateStore};
use std::collections::HashMap;

/// Rows kept for presentation before the cache is cleared and refilled.
const MAX_CACHED_ROWS: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FilterKey {
    folded_search: String,
    state: Option<String>,
    generation: u64,
    page_matches: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueueReadRequest {
    pub(crate) project_id: Option<String>,
    pub(crate) len: usize,
    pub(crate) generation: u64,
    pub(crate) search: String,
    pub(crate) state_filter: String,
    pub(crate) page_matches: bool,
    pub(crate) page_range: std::ops::Range<usize>,
    pub(crate) load_rowids: Vec<i64>,
    pub(crate) transient: HashMap<String, String>,
    pub(crate) selected_ids: Vec<String>,
    pub(crate) all_selected: bool,
    pub(crate) selection_summary_requested: bool,
}

pub(crate) struct QueueReadProjection {
    pub(crate) project_id: Option<String>,
    pub(crate) generation: u64,
    folded_search: String,
    state: Option<String>,
    page_matches: bool,
    visible: Option<Vec<i64>>,
    visible_count: usize,
    visible_paged: bool,
    filter_changed: bool,
    pub(crate) filter_error: Option<String>,
    page_start: usize,
    page: Vec<i64>,
    rows: Vec<QueueRow>,
    pub(crate) selection_summary: Option<Result<QueueSelectionSummary, String>>,
    selected_ids: Vec<String>,
    all_selected: bool,
    queue_summary: Result<BulkQueueSummary, String>,
}

/// Durable, queue-wide facts for the command center.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct QueueFleetFacts {
    /// Messages placed at the destination by runs with durable evidence.
    pub(crate) messages: u64,
    pub(crate) bytes: u64,
    /// Mailboxes those totals cover.
    pub(crate) evidenced: usize,
    pub(crate) hosts: Vec<String>,
}

#[derive(Default)]
pub(crate) struct MailboxQueue {
    project_id: Option<String>,
    len: usize,
    generation: u64,
    filter: Option<FilterKey>,
    visible: Vec<i64>,
    visible_count: usize,
    filter_error: Option<String>,
    /// The unfiltered and unselected filtered views are paged directly from
    /// SQLite; explicit selection and transient overlays retain matching IDs.
    visible_paged: bool,
    rows: HashMap<i64, QueueRow>,
    /// Row IDs fetched asynchronously for the current virtualized viewport.
    pages: HashMap<usize, Vec<i64>>,
    rowid_by_id: HashMap<String, i64>,
    /// Presentation-only states of rows during a run: a claimed row waiting
    /// out a retry backoff is `retrying` while its durable state stays
    /// `running`. Queued, running, and terminal states are durable.
    transient: HashMap<String, String>,
    pub(crate) session_secrets: SessionSecrets,
    /// Credential binding fingerprints recorded by a successful dry run in
    /// this session. Live admission requires an unchanged binding.
    pub(crate) preflight_credentials: HashMap<String, String>,
    summary: Option<(u64, BulkQueueSummary)>,
    summary_error: Option<String>,
    selection_summary: Option<(u64, QueueSelectionSummary)>,
    selection_summary_key: Option<(Vec<String>, bool)>,
    selection_summary_error: Option<String>,
    /// Command-center facts, recomputed only when the queue generation moves.
    fleet: Option<(u64, QueueFleetFacts)>,
}

impl MailboxQueue {
    pub(crate) fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Show the durable batch `project_id`, which holds `len` rows.
    pub(crate) fn attach(&mut self, project_id: String, len: usize) {
        let same = self.project_id.as_deref() == Some(project_id.as_str());
        if !same {
            self.session_secrets.clear();
            self.preflight_credentials.clear();
            self.transient.clear();
        }
        self.project_id = Some(project_id);
        self.len = len;
        self.invalidate();
    }

    /// Stop showing a queue. The durable project is left untouched.
    pub(crate) fn detach(&mut self) {
        *self = Self {
            generation: self.generation.wrapping_add(1),
            ..Self::default()
        };
    }

    /// Durable rows changed: drop cached rows, the filter, and the summary.
    pub(crate) fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.filter = None;
        self.rows.clear();
        self.pages.clear();
        self.rowid_by_id.clear();
        self.visible.clear();
        self.visible_count = 0;
        self.filter_error = None;
        self.visible_paged = false;
        self.summary = None;
        self.summary_error = None;
        self.selection_summary = None;
        self.selection_summary_key = None;
        self.fleet = None;
    }

    /// One row's durable state changed. Cached data for it is dropped; the
    /// filtered index is recomputed on next use only when a state filter
    /// could have changed membership.
    pub(crate) fn note_row_changed(&mut self, job_id: &str) {
        if let Some(rowid) = self.rowid_by_id.remove(job_id) {
            self.rows.remove(&rowid);
        }
        self.summary = None;
        self.summary_error = None;
        self.generation = self.generation.wrapping_add(1);
        if self
            .filter
            .as_ref()
            .is_some_and(|filter| filter.state.is_none())
            && let Some(filter) = self.filter.as_mut()
        {
            // Search membership does not depend on state.
            filter.generation = self.generation;
        }
    }

    pub(crate) fn set_transient(&mut self, job_id: &str, state: &str) {
        self.transient.insert(job_id.to_owned(), state.to_owned());
        self.note_row_changed(job_id);
    }

    pub(crate) fn clear_transient(&mut self, job_id: &str) {
        self.transient.remove(job_id);
        self.note_row_changed(job_id);
    }

    pub(crate) fn clear_all_transient(&mut self) {
        if !self.transient.is_empty() {
            self.transient.clear();
            self.invalidate();
        }
    }

    /// The state to present for `row`: a claimed row's transient state, or
    /// its durable (or `imported`) state.
    pub(crate) fn presented_state<'a>(&'a self, row: &'a QueueRow) -> &'a str {
        self.transient
            .get(&row.id)
            .map(String::as_str)
            .unwrap_or(row.state.as_str())
    }

    pub(crate) fn transient_state(&self, job_id: &str) -> Option<&str> {
        self.transient.get(job_id).map(String::as_str)
    }

    pub(crate) fn transient_count(&self, state: &str) -> usize {
        self.transient
            .values()
            .filter(|value| *value == state)
            .count()
    }

    /// Bring the filtered row index up to date. Returns whether it changed.
    #[cfg(test)]
    pub(crate) fn refresh_filter(
        &mut self,
        store: &StateStore,
        search: &str,
        state_filter: &str,
    ) -> Result<bool, String> {
        self.refresh_filter_with_paging(store, search, state_filter, false)
    }

    /// Filter rows in SQLite and retain only the visible viewport index when
    /// browsing without an explicit selection. Selection operations can ask
    /// for the materialized index so their membership remains exact.
    pub(crate) fn refresh_filter_with_paging(
        &mut self,
        store: &StateStore,
        search: &str,
        state_filter: &str,
        page_matches: bool,
    ) -> Result<bool, String> {
        let Some(project_id) = self.project_id.clone() else {
            let changed = !self.visible.is_empty() || self.visible_count != 0;
            self.visible.clear();
            self.visible_count = 0;
            self.visible_paged = false;
            self.filter_error = None;
            return Ok(changed);
        };
        let key = FilterKey {
            folded_search: crate::ui::fold_search_text(search.trim()),
            state: match state_filter {
                "" | "all" => None,
                state => Some(state.to_owned()),
            },
            generation: self.generation,
            page_matches,
        };
        if self.filter.as_ref() == Some(&key) {
            return Ok(false);
        }
        if self.transient.is_empty()
            && (key.folded_search.is_empty() && key.state.is_none() || page_matches)
        {
            // Release a potentially large filtered-ID allocation when an
            // operator returns from explicit selection to paged browsing.
            self.visible = Vec::new();
            self.visible_count = match if key.folded_search.is_empty() && key.state.is_none() {
                store.queue_len(&project_id)
            } else {
                store.queue_rowid_count(&project_id, &key.folded_search, key.state.as_deref())
            } {
                Ok(count) => count,
                Err(error) => {
                    return self.filter_failed(
                        key,
                        format!("Could not count matching mailbox rows: {error}"),
                    );
                }
            };
            self.visible_paged = true;
            self.filter = Some(key);
            self.filter_error = None;
            return Ok(true);
        }
        self.visible =
            match store.queue_rowids(&project_id, &key.folded_search, key.state.as_deref()) {
                Ok(rowids) => rowids,
                Err(error) => {
                    return self.filter_failed(
                        key,
                        format!("Could not filter the mailbox queue: {error}"),
                    );
                }
            };
        // A transient state is not durable, so a state filter must also
        // honour claimed rows' presented state.
        if let Some(state) = key.state.as_deref()
            && !self.transient.is_empty()
        {
            let claimed = self.transient.keys().cloned().collect::<Vec<_>>();
            let plans = match store.queue_plans(&project_id, &claimed) {
                Ok(plans) => plans,
                Err(error) => {
                    return self.filter_failed(
                        key,
                        format!("Could not filter the mailbox queue: {error}"),
                    );
                }
            };
            for plan in plans {
                let presented = self.transient.get(&plan.id).map(String::as_str);
                let matches = presented.is_some_and(|presented| {
                    crate::core::state_filter_members(state).contains(&presented)
                });
                let matches_search = if matches && self.visible.binary_search(&plan.rowid).is_err()
                {
                    match store.queue_row_matches(&project_id, plan.rowid, &key.folded_search) {
                        Ok(matches) => matches,
                        Err(error) => {
                            return self.filter_failed(
                                key,
                                format!("Could not filter the mailbox queue: {error}"),
                            );
                        }
                    }
                } else {
                    false
                };
                match (self.visible.binary_search(&plan.rowid), matches) {
                    (Ok(index), false) => {
                        self.visible.remove(index);
                    }
                    (Err(index), true) if matches_search => {
                        self.visible.insert(index, plan.rowid);
                    }
                    _ => {}
                }
            }
        }
        self.visible_count = self.visible.len();
        self.visible_paged = false;
        self.filter = Some(key);
        self.filter_error = None;
        Ok(true)
    }

    fn filter_failed(&mut self, key: FilterKey, error: String) -> Result<bool, String> {
        self.visible = Vec::new();
        self.visible_count = 0;
        self.visible_paged = false;
        self.filter = Some(key);
        self.filter_error = Some(error.clone());
        Err(error)
    }

    pub(crate) fn filter_error(&self) -> Option<&str> {
        self.filter_error.as_deref()
    }

    pub(crate) fn retry_filter(&mut self) {
        self.filter = None;
        self.filter_error = None;
        self.visible.clear();
        self.visible_count = 0;
        self.visible_paged = false;
    }

    /// Row IDs matching the current filter, ascending.
    pub(crate) fn visible(&self) -> &[i64] {
        &self.visible
    }

    pub(crate) fn visible_count(&self) -> usize {
        self.visible_count
    }

    pub(crate) fn is_unfiltered_paged(&self) -> bool {
        self.visible_paged
            && self
                .filter
                .as_ref()
                .is_some_and(|filter| filter.folded_search.is_empty() && filter.state.is_none())
    }

    /// Read a rowid range for virtualization. The default all-mailboxes view
    /// asks SQLite only for the requested page; filtered modes slice their
    /// bounded selection index.
    pub(crate) fn visible_page(
        &self,
        store: &StateStore,
        range: std::ops::Range<usize>,
    ) -> Result<Vec<i64>, String> {
        let start = range.start.min(self.visible_count);
        let end = range.end.min(self.visible_count).max(start);
        if self.visible_paged {
            let Some(project_id) = self.project_id.as_deref() else {
                return Ok(Vec::new());
            };
            let Some(filter) = self.filter.as_ref() else {
                return Ok(Vec::new());
            };
            store
                .queue_rowid_page(
                    project_id,
                    &filter.folded_search,
                    filter.state.as_deref(),
                    start,
                    end - start,
                )
                .map_err(|error| format!("Could not read mailbox queue page: {error}"))
        } else {
            Ok(self.visible[start..end].to_vec())
        }
    }

    /// Materialize matching IDs only for an explicit operator action such as
    /// Select visible. Ordinary rendering always uses `visible_page`.
    pub(crate) fn materialize_visible(&self, store: &StateStore) -> Result<Vec<i64>, String> {
        if !self.visible_paged {
            return Ok(self.visible.clone());
        }
        let Some(project_id) = self.project_id.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(filter) = self.filter.as_ref() else {
            return Ok(Vec::new());
        };
        store
            .queue_rowids(project_id, &filter.folded_search, filter.state.as_deref())
            .map_err(|error| format!("Could not read visible mailbox rows: {error}"))
    }

    /// Whether `rowid` is in the current view. A filtered paged view keeps no
    /// membership index, so it answers conservatively (not visible) rather
    /// than hiding a filtered-out selection from a safety confirmation.
    pub(crate) fn is_visible(&self, rowid: i64) -> bool {
        self.is_unfiltered_paged() || self.visible.binary_search(&rowid).is_ok()
    }

    /// Make sure `rowids` are cached, fetching any that are not.
    pub(crate) fn load(&mut self, store: &StateStore, rowids: &[i64]) -> Result<(), String> {
        let Some(project_id) = self.project_id.as_deref() else {
            return Ok(());
        };
        let missing = rowids
            .iter()
            .copied()
            .filter(|rowid| !self.rows.contains_key(rowid))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(());
        }
        if self.rows.len() + missing.len() > MAX_CACHED_ROWS {
            self.rows.clear();
            self.rowid_by_id.clear();
            self.pages.clear();
        }
        for row in store
            .queue_rows(project_id, &missing)
            .map_err(|error| format!("Could not read mailbox rows: {error}"))?
        {
            self.rowid_by_id.insert(row.id.clone(), row.rowid);
            self.rows.insert(row.rowid, row);
        }
        Ok(())
    }

    /// A cached row; call `load` first.
    pub(crate) fn cached(&self, rowid: i64) -> Option<&QueueRow> {
        self.rows.get(&rowid)
    }

    pub(crate) fn cached_visible_page(&self, range: std::ops::Range<usize>) -> Option<Vec<i64>> {
        let start = range.start.min(self.visible_count);
        let end = range.end.min(self.visible_count).max(start);
        if !self.visible_paged {
            return Some(self.visible.get(start..end)?.to_vec());
        }
        self.pages
            .iter()
            .find(|(page_start, ids)| {
                **page_start <= start && page_start.saturating_add(ids.len()) >= end
            })
            .map(|(page_start, ids)| ids[start - *page_start..end - *page_start].to_vec())
    }

    pub(crate) fn apply_read_projection(&mut self, result: QueueReadProjection) {
        if result.filter_changed {
            self.filter = Some(FilterKey {
                folded_search: result.folded_search,
                state: result.state,
                generation: result.generation,
                page_matches: result.page_matches,
            });
            self.visible = result.visible.unwrap_or_default();
            self.pages.clear();
        }
        self.visible_count = result.visible_count;
        self.visible_paged = result.visible_paged;
        self.filter_error = result.filter_error;
        if self.rows.len().saturating_add(result.rows.len()) > MAX_CACHED_ROWS {
            self.rows.clear();
            self.rowid_by_id.clear();
            self.pages.clear();
        }
        if !result.page.is_empty() || result.page_start == 0 {
            self.pages.insert(result.page_start, result.page);
        }
        for row in result.rows {
            self.rowid_by_id.insert(row.id.clone(), row.rowid);
            self.rows.insert(row.rowid, row);
        }
        self.selection_summary_error = None;
        match result.selection_summary {
            Some(Ok(summary)) => {
                self.selection_summary = Some((self.generation, summary));
                self.selection_summary_key = Some((result.selected_ids, result.all_selected));
            }
            Some(Err(error)) => self.selection_summary_error = Some(error),
            None => {}
        }
        match result.queue_summary {
            Ok(summary) => {
                self.summary = Some((self.generation, summary));
                self.summary_error = None;
            }
            Err(error) => self.summary_error = Some(error),
        }
    }

    pub(crate) fn current_filter_matches(
        &self,
        search: &str,
        state_filter: &str,
        page_matches: bool,
    ) -> bool {
        self.filter.as_ref().is_some_and(|filter| {
            filter.folded_search == crate::ui::fold_search_text(search.trim())
                && filter.state.as_deref().unwrap_or("all")
                    == match state_filter {
                        "" => "all",
                        state => state,
                    }
                && filter.generation == self.generation
                && filter.page_matches == page_matches
        })
    }

    pub(crate) fn selection_projection(&self) -> Option<&QueueSelectionSummary> {
        self.selection_summary.as_ref().map(|(_, summary)| summary)
    }

    pub(crate) fn selection_projection_error(&self) -> Option<&str> {
        self.selection_summary_error.as_deref()
    }

    pub(crate) fn selection_projection_matches(
        &self,
        selected_ids: &[String],
        all_selected: bool,
    ) -> bool {
        self.selection_summary_key
            .as_ref()
            .is_some_and(|(ids, all)| *all == all_selected && ids == selected_ids)
    }

    pub(crate) fn cached_queue_summary(&self) -> Option<BulkQueueSummary> {
        self.summary
            .as_ref()
            .filter(|(generation, _)| *generation == self.generation)
            .map(|(_, summary)| *summary)
    }

    pub(crate) fn queue_summary_error(&self) -> Option<&str> {
        self.summary_error.as_deref()
    }

    pub(crate) fn transient_snapshot(&self) -> HashMap<String, String> {
        self.transient.clone()
    }

    pub(crate) fn read_projection(
        &mut self,
        store: &StateStore,
        request: QueueReadRequest,
    ) -> QueueReadProjection {
        if self.project_id != request.project_id {
            if let Some(project_id) = request.project_id.clone() {
                self.attach(project_id, request.len);
            } else {
                self.detach();
            }
            self.generation = request.generation;
        }
        if self.generation != request.generation || self.transient != request.transient {
            self.invalidate();
            self.generation = request.generation;
            self.transient = request.transient.clone();
        }
        if self.filter_error.is_some() {
            self.retry_filter();
        }
        self.len = request.len;
        let queue_summary = self
            .summary(store)
            .map_err(|error| format!("Could not read mailbox queue status: {error}"));
        let filter_result = self.refresh_filter_with_paging(
            store,
            &request.search,
            &request.state_filter,
            request.page_matches,
        );
        let filter_changed = filter_result.as_ref().is_ok_and(|changed| *changed);
        let mut filter_error = filter_result.as_ref().err().cloned();
        let (page_start, page, rows, selection_summary) = if filter_error.is_none() {
            let range = request.page_range.clone();
            match self.visible_page(store, range.clone()) {
                Err(error) => {
                    filter_error = Some(error);
                    (range.start, Vec::new(), Vec::new(), None)
                }
                Ok(page) => {
                    let mut rowids_to_load = page.clone();
                    rowids_to_load.extend(request.load_rowids.iter().copied());
                    rowids_to_load.sort_unstable();
                    rowids_to_load.dedup();
                    match self.load(store, &rowids_to_load) {
                        Err(error) => {
                            filter_error = Some(error);
                            (range.start, Vec::new(), Vec::new(), None)
                        }
                        Ok(()) => {
                            let rows = rowids_to_load
                                .iter()
                                .filter_map(|rowid| self.cached(*rowid).cloned())
                                .collect();
                            let selection_summary = request
                                .selection_summary_requested
                                .then_some(request.project_id.as_deref())
                                .flatten()
                                .map(|project_id| {
                                    store
                                .queue_selection_summary(
                                    project_id,
                                    &request.selected_ids,
                                    request.all_selected,
                                    self.visible(),
                                )
                                .map_err(|error| {
                                    format!("Could not read the selected mailbox summary: {error}")
                                })
                                });
                            (range.start, page, rows, selection_summary)
                        }
                    }
                }
            }
        } else {
            (request.page_range.start, Vec::new(), Vec::new(), None)
        };
        QueueReadProjection {
            project_id: request.project_id,
            generation: request.generation,
            folded_search: crate::ui::fold_search_text(request.search.trim()),
            state: match request.state_filter.as_str() {
                "" | "all" => None,
                state => Some(state.to_owned()),
            },
            page_matches: request.page_matches,
            visible: filter_changed.then(|| self.visible.clone()),
            visible_count: self.visible_count,
            visible_paged: self.visible_paged,
            filter_changed,
            filter_error,
            page_start,
            page,
            rows,
            selection_summary,
            selected_ids: request.selected_ids,
            all_selected: request.all_selected,
            queue_summary,
        }
    }

    /// One row by job ID, from the cache or the ledger.
    pub(crate) fn row_by_id(
        &mut self,
        store: &StateStore,
        job_id: &str,
    ) -> rusqlite::Result<Option<QueueRow>> {
        if let Some(row) = self
            .rowid_by_id
            .get(job_id)
            .and_then(|rowid| self.rows.get(rowid))
        {
            return Ok(Some(row.clone()));
        }
        let Some(project_id) = self.project_id.as_deref() else {
            return Ok(None);
        };
        let Some(row) = store.queue_row(project_id, job_id)? else {
            return Ok(None);
        };
        self.rowid_by_id.insert(row.id.clone(), row.rowid);
        self.rows.insert(row.rowid, row.clone());
        Ok(Some(row))
    }

    /// Verified transfer totals and the queue's endpoint hosts, cached per
    /// generation so the command center does not rescan 100k rows a frame.
    /// Changes whenever durable queue state may have changed; read models
    /// keyed by it are recomputed only when it moves.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn fleet_facts(&mut self, store: &StateStore) -> Result<QueueFleetFacts, String> {
        if let Some((generation, facts)) = &self.fleet
            && *generation == self.generation
        {
            return Ok(facts.clone());
        }
        let Some(project_id) = self.project_id.clone() else {
            return Ok(QueueFleetFacts::default());
        };
        let read = |error: rusqlite::Error| format!("Could not read migration totals: {error}");
        let (messages, bytes, evidenced) =
            store.queue_transfer_totals(&project_id).map_err(read)?;
        let hosts = store.queue_endpoint_hosts(&project_id).map_err(read)?;
        let facts = QueueFleetFacts {
            messages,
            bytes,
            evidenced,
            hosts,
        };
        self.fleet = Some((self.generation, facts.clone()));
        Ok(facts)
    }

    /// Queue health counts, with claimed rows counted by presented state.
    pub(crate) fn summary(&mut self, store: &StateStore) -> Result<BulkQueueSummary, String> {
        if let Some((generation, summary)) = self.summary
            && generation == self.generation
        {
            return Ok(summary);
        }
        let Some(project_id) = self.project_id.clone() else {
            return Ok(BulkQueueSummary::default());
        };
        let mut counts = store
            .queue_state_counts(&project_id)
            .map_err(|error| format!("Could not read mailbox queue status: {error}"))?
            .into_iter()
            .collect::<HashMap<_, _>>();
        if !self.transient.is_empty() {
            let claimed = self.transient.keys().cloned().collect::<Vec<_>>();
            for plan in store
                .queue_plans(&project_id, &claimed)
                .map_err(|error| format!("Could not read mailbox queue status: {error}"))?
            {
                if let Some(count) = counts.get_mut(&plan.state) {
                    *count = count.saturating_sub(1);
                }
                if let Some(presented) = self.transient.get(&plan.id) {
                    *counts.entry(presented.clone()).or_default() += 1;
                }
            }
        }
        let summary = BulkQueueSummary::from_state_counts(
            counts.iter().map(|(state, count)| (state.as_str(), *count)),
        );
        self.summary = Some((self.generation, summary));
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bulk_import::BulkJob;
    use crate::controller::queue::persist_imported_queue;

    pub(crate) fn imported(store: &StateStore, count: usize) -> MailboxQueue {
        let jobs = (0..count)
            .map(|index| {
                let mut form = crate::Form::default();
                form.profile.source_host = "old.example".into();
                form.profile.source_user = format!("user{index}@old.example");
                form.profile.destination_host = "new.example".into();
                form.profile.destination_user = format!("user{index}@new.example");
                BulkJob::from_form(format!("Row {index}"), form, "imported".into())
            })
            .collect();
        let imported = persist_imported_queue(store, jobs, &crate::Profile::default()).unwrap();
        let mut queue = MailboxQueue::default();
        queue.attach(imported.project_id, imported.len);
        queue
    }

    #[test]
    fn background_projection_loads_filtered_page_and_selection_summary_together() {
        let store = StateStore::in_memory().unwrap();
        let queue = imported(&store, 20);
        let project_id = queue.project_id().unwrap().to_owned();
        let rowids = store.queue_rowids(&project_id, "", None).unwrap();
        let rows = store.queue_rows(&project_id, &rowids).unwrap();
        let selected_ids = vec![rows[10].id.clone()];
        let request = QueueReadRequest {
            project_id: Some(project_id),
            len: queue.len(),
            generation: queue.generation(),
            search: "user1".into(),
            state_filter: "all".into(),
            page_matches: false,
            page_range: 0..3,
            load_rowids: Vec::new(),
            transient: Default::default(),
            selected_ids,
            all_selected: false,
            selection_summary_requested: true,
        };

        let mut worker_queue = MailboxQueue::default();
        let projection = worker_queue.read_projection(&store, request);

        assert_eq!(projection.visible_count, 11);
        assert_eq!(projection.page.len(), 3);
        assert_eq!(projection.rows.len(), 3);
        assert_eq!(
            projection
                .selection_summary
                .unwrap()
                .unwrap()
                .selected_loaded,
            1
        );
    }

    #[test]
    fn filter_index_and_row_cache_follow_durable_state() {
        let store = StateStore::in_memory().unwrap();
        let mut queue = imported(&store, 20);
        assert!(queue.refresh_filter(&store, "", "all").unwrap());
        assert_eq!(queue.visible_count(), 20);
        assert!(queue.is_unfiltered_paged());
        let first_page = queue.visible_page(&store, 0..5).unwrap();
        let matching = store
            .queue_rowids(queue.project_id().unwrap(), "", None)
            .unwrap();
        assert_eq!(first_page, matching[..5]);
        assert_eq!(queue.visible_page(&store, 15..25).unwrap(), matching[15..]);
        assert_eq!(queue.materialize_visible(&store).unwrap(), matching);
        assert!(!queue.refresh_filter(&store, "", "all").unwrap());
        assert!(queue.refresh_filter(&store, "USER1", "all").unwrap());
        assert_eq!(queue.visible().len(), 11);
        let first = queue.visible()[0];
        queue.load(&store, &[first]).unwrap();
        let row = queue.cached(first).unwrap().clone();
        assert_eq!(queue.presented_state(&row), "imported");

        // A row waiting out a retry is presented and filtered by that state.
        queue.set_transient(&row.id, "retrying");
        let row = queue.row_by_id(&store, &row.id).unwrap().unwrap();
        assert_eq!(queue.presented_state(&row), "retrying");
        assert!(queue.refresh_filter(&store, "", "retrying").unwrap());
        assert_eq!(queue.visible(), [row.rowid]);
        assert!(queue.refresh_filter(&store, "", "imported").unwrap());
        assert_eq!(queue.visible().len(), 19);
        assert_eq!(queue.summary(&store).unwrap().imported, 19);
        queue.clear_transient(&row.id);
        assert!(queue.refresh_filter(&store, "", "retrying").unwrap());
        assert!(queue.visible().is_empty());
        assert_eq!(queue.visible_count(), 0);
        assert_eq!(queue.summary(&store).unwrap().imported, 20);
    }

    #[test]
    fn filtered_browsing_pages_matches_until_selection_needs_materialization() {
        let store = StateStore::in_memory().unwrap();
        let mut queue = imported(&store, 200);

        assert!(
            queue
                .refresh_filter_with_paging(&store, "USER1", "all", true)
                .unwrap()
        );
        assert!(queue.visible().is_empty());
        assert!(queue.visible_paged);
        assert!(!queue.is_unfiltered_paged());
        let expected = store
            .queue_rowids(queue.project_id().unwrap(), "user1", None)
            .unwrap();
        assert_eq!(queue.visible_count(), expected.len());
        assert_eq!(queue.visible_page(&store, 0..7).unwrap(), expected[..7]);
        assert_eq!(queue.materialize_visible(&store).unwrap(), expected);

        assert!(
            queue
                .refresh_filter_with_paging(&store, "USER1", "all", false)
                .unwrap()
        );
        assert!(!queue.visible_paged);
        assert_eq!(queue.visible(), expected);
        assert!(queue.visible.capacity() >= expected.len());
        assert!(
            queue
                .refresh_filter_with_paging(&store, "USER1", "all", true)
                .unwrap()
        );
        assert!(queue.visible_paged);
        assert_eq!(queue.visible.capacity(), 0);

        let project_id = queue.project_id().unwrap().to_owned();
        for id in store.mailbox_ids(&project_id).unwrap().into_iter().take(10) {
            store.force_mailbox_state(&id, "attention").unwrap();
        }
        queue.invalidate();
        assert!(
            queue
                .refresh_filter_with_paging(&store, "", "attention", true)
                .unwrap()
        );
        let expected_attention = store
            .queue_rowids(&project_id, "", Some("attention"))
            .unwrap();
        assert_eq!(queue.visible_count(), 10);
        assert_eq!(queue.visible_count(), expected_attention.len());
        assert_eq!(
            queue.visible_page(&store, 0..20).unwrap(),
            expected_attention
        );
    }

    #[test]
    fn command_center_buckets_and_their_filters_select_the_same_rows() {
        let store = StateStore::in_memory().unwrap();
        let mut queue = imported(&store, 8);
        let project_id = queue.project_id().unwrap().to_owned();
        let ids = store.mailbox_ids(&project_id).unwrap();
        for (id, state) in ids.iter().zip([
            "attention",
            "failed",
            "verification_difference",
            "verified",
            "verified_with_exceptions",
            "running",
            "delta_required",
        ]) {
            store.force_mailbox_state(id, state).unwrap();
        }
        // The running row is presented as retrying after provider pushback.
        queue.set_transient(&ids[5], "retrying");
        queue.invalidate();
        let counts = queue.summary(&store).unwrap().command_center();
        assert_eq!(
            (
                counts.needs_attention,
                counts.completed,
                counts.retrying,
                counts.migrating,
                counts.waiting,
                counts.total
            ),
            (4, 1, 1, 0, 2, 8)
        );
        for (filter, expected) in [
            ("needs_attention", counts.needs_attention),
            ("completed", counts.completed),
            ("retrying", counts.retrying),
            ("migrating", counts.migrating),
            ("waiting", counts.waiting),
        ] {
            queue.retry_filter();
            queue.refresh_filter(&store, "", filter).unwrap();
            assert_eq!(queue.visible().len(), expected, "{filter}");
        }
    }

    #[test]
    fn queue_summary_and_filter_fail_explicitly_when_durable_reads_fail() {
        let store = StateStore::in_memory().unwrap();
        let mut queue = imported(&store, 3);
        store.hide_mailbox_jobs_for_test().unwrap();

        let summary_error = queue.summary(&store).unwrap_err();
        assert!(summary_error.contains("queue status"), "{summary_error}");

        let filter_error = queue.refresh_filter(&store, "user", "all").unwrap_err();
        assert!(
            filter_error.contains("filter the mailbox queue"),
            "{filter_error}"
        );
        assert!(queue.filter_error().is_some());
        assert!(queue.visible().is_empty());
        assert_eq!(queue.visible_count(), 0);
    }

    #[test]
    fn detach_forgets_session_only_data() {
        let store = StateStore::in_memory().unwrap();
        let mut queue = imported(&store, 2);
        queue
            .preflight_credentials
            .insert("job".into(), "fingerprint".into());
        let project = queue.project_id().unwrap().to_owned();
        queue.attach(project, 2);
        assert_eq!(queue.preflight_credentials.len(), 1);
        queue.detach();
        assert!(queue.is_empty() && queue.preflight_credentials.is_empty());
        assert!(queue.project_id().is_none());
    }
}
