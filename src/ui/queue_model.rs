//! The Mailboxes view's read model over the durable batch queue.
//!
//! The queue itself lives in SQLite (`core::queue`). The GUI keeps only what
//! a frame needs: the row IDs matching the current filter (the virtual-row
//! index), a bounded cache of rows that have been on screen, and two small
//! overlays that are not durable by design: presentation states of rows a
//! running batch has claimed, and session-only data that must never reach the
//! ledger (plaintext-import passwords and preflight credential fingerprints).

use crate::controller::{BulkQueueSummary, queue::SessionSecrets};
use crate::core::{QueueRow, StateStore};
use std::collections::HashMap;

/// Rows kept for presentation before the cache is cleared and refilled.
const MAX_CACHED_ROWS: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FilterKey {
    folded_search: String,
    state: Option<String>,
    generation: u64,
}

#[derive(Default)]
pub(crate) struct MailboxQueue {
    project_id: Option<String>,
    len: usize,
    generation: u64,
    filter: Option<FilterKey>,
    visible: Vec<i64>,
    rows: HashMap<i64, QueueRow>,
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
        self.rowid_by_id.clear();
        self.summary = None;
    }

    /// One row's durable state changed. Cached data for it is dropped; the
    /// filtered index is recomputed on next use only when a state filter
    /// could have changed membership.
    pub(crate) fn note_row_changed(&mut self, job_id: &str) {
        if let Some(rowid) = self.rowid_by_id.remove(job_id) {
            self.rows.remove(&rowid);
        }
        self.summary = None;
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

    /// The state to present for a job whose durable/imported state is
    /// `state`.
    pub(crate) fn presented_state_of<'a>(&'a self, job_id: &str, state: &'a str) -> &'a str {
        self.transient
            .get(job_id)
            .map(String::as_str)
            .unwrap_or(state)
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
    pub(crate) fn refresh_filter(
        &mut self,
        store: &StateStore,
        search: &str,
        state_filter: &str,
    ) -> Result<bool, String> {
        let Some(project_id) = self.project_id.clone() else {
            let changed = !self.visible.is_empty();
            self.visible.clear();
            return Ok(changed);
        };
        let key = FilterKey {
            folded_search: crate::ui::fold_search_text(search.trim()),
            state: match state_filter {
                "" | "all" => None,
                state => Some(state.to_owned()),
            },
            generation: self.generation,
        };
        if self.filter.as_ref() == Some(&key) {
            return Ok(false);
        }
        self.visible = store
            .queue_rowids(&project_id, &key.folded_search, key.state.as_deref())
            .map_err(|error| format!("Could not filter the mailbox queue: {error}"))?;
        // A transient state is not durable, so a state filter must also
        // honour claimed rows' presented state.
        if let Some(state) = key.state.as_deref()
            && !self.transient.is_empty()
        {
            let claimed = self.transient.keys().cloned().collect::<Vec<_>>();
            let plans = store
                .queue_plans(&project_id, &claimed)
                .map_err(|error| format!("Could not filter the mailbox queue: {error}"))?;
            for plan in plans {
                let presented = self.transient.get(&plan.id).map(String::as_str);
                let matches = presented == Some(state);
                match (self.visible.binary_search(&plan.rowid), matches) {
                    (Ok(index), false) => {
                        self.visible.remove(index);
                    }
                    (Err(index), true)
                        if store
                            .queue_row_matches(&project_id, plan.rowid, &key.folded_search)
                            .unwrap_or(false) =>
                    {
                        self.visible.insert(index, plan.rowid);
                    }
                    _ => {}
                }
            }
        }
        self.filter = Some(key);
        Ok(true)
    }

    /// Row IDs matching the current filter, ascending.
    pub(crate) fn visible(&self) -> &[i64] {
        &self.visible
    }

    pub(crate) fn is_visible(&self, rowid: i64) -> bool {
        self.visible.binary_search(&rowid).is_ok()
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

    /// One row by job ID, from the cache or the ledger.
    pub(crate) fn row_by_id(&mut self, store: &StateStore, job_id: &str) -> Option<QueueRow> {
        if let Some(row) = self
            .rowid_by_id
            .get(job_id)
            .and_then(|rowid| self.rows.get(rowid))
        {
            return Some(row.clone());
        }
        let row = store
            .queue_row(self.project_id.as_deref()?, job_id)
            .ok()??;
        self.rowid_by_id.insert(row.id.clone(), row.rowid);
        self.rows.insert(row.rowid, row.clone());
        Some(row)
    }

    /// Install queue health counts computed by a full scan of presented
    /// states, sparing a separate counting query this generation.
    pub(crate) fn store_summary(&mut self, counts: &HashMap<String, usize>) {
        let summary = BulkQueueSummary::from_state_counts(
            counts.iter().map(|(state, count)| (state.as_str(), *count)),
        );
        self.summary = Some((self.generation, summary));
    }

    /// Queue health counts, with claimed rows counted by presented state.
    pub(crate) fn summary(&mut self, store: &StateStore) -> BulkQueueSummary {
        if let Some((generation, summary)) = self.summary
            && generation == self.generation
        {
            return summary;
        }
        let Some(project_id) = self.project_id.clone() else {
            return BulkQueueSummary::default();
        };
        let mut counts = store
            .queue_state_counts(&project_id)
            .unwrap_or_default()
            .into_iter()
            .collect::<HashMap<_, _>>();
        if !self.transient.is_empty() {
            let claimed = self.transient.keys().cloned().collect::<Vec<_>>();
            for plan in store.queue_plans(&project_id, &claimed).unwrap_or_default() {
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
        summary
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
    fn filter_index_and_row_cache_follow_durable_state() {
        let store = StateStore::in_memory().unwrap();
        let mut queue = imported(&store, 20);
        assert!(queue.refresh_filter(&store, "", "all").unwrap());
        assert_eq!(queue.visible().len(), 20);
        assert!(!queue.refresh_filter(&store, "", "all").unwrap());
        assert!(queue.refresh_filter(&store, "USER1", "all").unwrap());
        assert_eq!(queue.visible().len(), 11);
        let first = queue.visible()[0];
        queue.load(&store, &[first]).unwrap();
        let row = queue.cached(first).unwrap().clone();
        assert_eq!(queue.presented_state(&row), "imported");

        // A row waiting out a retry is presented and filtered by that state.
        queue.set_transient(&row.id, "retrying");
        let row = queue.row_by_id(&store, &row.id).unwrap();
        assert_eq!(queue.presented_state(&row), "retrying");
        assert!(queue.refresh_filter(&store, "", "retrying").unwrap());
        assert_eq!(queue.visible(), [row.rowid]);
        assert!(queue.refresh_filter(&store, "", "imported").unwrap());
        assert_eq!(queue.visible().len(), 19);
        assert_eq!(queue.summary(&store).imported, 19);
        queue.clear_transient(&row.id);
        assert!(queue.refresh_filter(&store, "", "retrying").unwrap());
        assert!(queue.visible().is_empty());
        assert_eq!(queue.summary(&store).imported, 20);
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
