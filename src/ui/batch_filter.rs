//! Search and state filtering for the batch mailbox cockpit.

use crate::App;
use crate::bulk_import::BulkJob;
use crate::ui::{contains_ascii_case_insensitive, display_state_key};

/// Filter the cached row projection without touching the owned queue forms.
/// Keeping this pure makes the large-batch cost measurable independently from
/// egui repainting and prevents accidental per-widget allocations.
pub(crate) fn filter_batch_indices<'a, I>(
    search_values: &[String],
    states: I,
    search: &str,
    state_filter: &str,
    visible_indices: &mut Vec<usize>,
) where
    I: Iterator<Item = &'a str>,
{
    let normalized_search = search.trim().to_ascii_lowercase();
    visible_indices.clear();
    for (index, (value, raw_state)) in search_values.iter().zip(states).enumerate() {
        let state = display_state_key(raw_state);
        let state_matches = state_filter.is_empty()
            || state_filter == "all"
            || state == state_filter
            || (state_filter == "delta_required" && state.contains("delta"))
            || (state_filter == "verification_difference" && state.contains("verification"));
        let search_matches = normalized_search.is_empty()
            || value.contains(&normalized_search)
            || (!normalized_search.is_ascii() && contains_ascii_case_insensitive(value, search));
        if state_matches && search_matches {
            visible_indices.push(index);
        }
    }
}

impl App {
    pub(crate) fn mailbox_matches_filter(&self, job: &BulkJob) -> bool {
        let state = job.state.to_ascii_lowercase().replace(' ', "_");
        if !self.bulk_state_filter.is_empty()
            && self.bulk_state_filter != "all"
            && state != self.bulk_state_filter
            && !(self.bulk_state_filter == "delta_required" && state.contains("delta"))
            && !(self.bulk_state_filter == "verification_difference"
                && state.contains("verification"))
        {
            return false;
        }
        let search = self.bulk_search.trim();
        search.is_empty()
            || [
                job.label.as_str(),
                job.form.profile.source_host.as_str(),
                job.form.profile.source_user.as_str(),
                job.form.profile.destination_host.as_str(),
                job.form.profile.destination_user.as_str(),
            ]
            .iter()
            .any(|value| contains_ascii_case_insensitive(value, search))
    }

    pub(crate) fn rebuild_bulk_search_values(&mut self) {
        self.bulk_search_values = self
            .bulk_jobs
            .iter()
            .map(|job| {
                [
                    job.label.as_str(),
                    job.form.profile.source_host.as_str(),
                    job.form.profile.source_user.as_str(),
                    job.form.profile.destination_host.as_str(),
                    job.form.profile.destination_user.as_str(),
                ]
                .join(" ")
                .to_ascii_lowercase()
            })
            .collect();
    }

    pub(crate) fn refresh_bulk_filter_cache(&mut self) {
        let raw_search = self.bulk_search.trim().to_owned();
        let cache_is_current = self.bulk_filter_cache_search == raw_search
            && self.bulk_filter_cache_state == self.bulk_state_filter
            && self.bulk_filter_cache_generation == self.bulk_jobs_generation
            && self.bulk_search_values.len() == self.bulk_jobs.len();
        if cache_is_current {
            return;
        }
        if self.bulk_search_values.len() != self.bulk_jobs.len() {
            self.rebuild_bulk_search_values();
        }
        let normalized_search = raw_search.to_ascii_lowercase();
        self.bulk_visible_indices.clear();
        // The default mailbox view is already the complete queue. Avoid
        // walking and re-evaluating every row after each durable state update
        // while a large batch is running; egui still virtualizes the table.
        if normalized_search.is_empty()
            && (self.bulk_state_filter.is_empty() || self.bulk_state_filter == "all")
        {
            self.bulk_visible_indices.extend(0..self.bulk_jobs.len());
            self.bulk_filter_cache_search = raw_search;
            self.bulk_filter_cache_state = self.bulk_state_filter.clone();
            self.bulk_filter_cache_generation = self.bulk_jobs_generation;
            return;
        }
        filter_batch_indices(
            &self.bulk_search_values,
            self.bulk_jobs.iter().map(|job| job.state.as_str()),
            &normalized_search,
            &self.bulk_state_filter,
            &mut self.bulk_visible_indices,
        );
        self.bulk_filter_cache_search = raw_search;
        self.bulk_filter_cache_state = self.bulk_state_filter.clone();
        self.bulk_filter_cache_generation = self.bulk_jobs_generation;
    }
}

#[cfg(test)]
mod tests {
    use super::filter_batch_indices;
    use std::collections::HashSet;
    use std::time::Instant;

    #[test]
    fn cached_filter_matches_state_and_search_semantics() {
        let values = vec![
            "alice@source.example destination.example".to_owned(),
            "bob@source.example destination.example".to_owned(),
        ];
        let states = ["ready".to_owned(), "attention".to_owned()];
        let mut visible = Vec::new();
        filter_batch_indices(
            &values,
            states.iter().map(String::as_str),
            "alice",
            "ready",
            &mut visible,
        );
        assert_eq!(visible, vec![0]);
        filter_batch_indices(
            &values,
            states.iter().map(String::as_str),
            "",
            "attention",
            &mut visible,
        );
        assert_eq!(visible, vec![1]);
    }

    #[test]
    #[ignore = "opt-in release UI scale benchmark; run scripts/benchmark-ui-scale.sh"]
    fn scale_ui_benchmark() {
        let rows = 100_000;
        let values = (0..rows)
            .map(|index| {
                format!("row {index} user{index}@source.example user{index}@destination.example")
            })
            .collect::<Vec<_>>();
        let mut states = vec!["ready".to_owned(); rows];
        let ids = (0..rows)
            .map(|index| format!("job-{index}"))
            .collect::<Vec<_>>();
        let mut visible = Vec::with_capacity(rows);
        let started = Instant::now();
        filter_batch_indices(
            &values,
            states.iter().map(String::as_str),
            "user50000",
            "all",
            &mut visible,
        );
        let filter_ms = started.elapsed().as_millis();
        assert_eq!(visible, vec![50_000]);

        let started = Instant::now();
        let selected = ids.iter().cloned().collect::<HashSet<_>>();
        let selection_all_ms = started.elapsed().as_millis();
        assert_eq!(selected.len(), rows);

        for state in states.iter_mut().take(1_000) {
            *state = "attention".into();
        }
        let started = Instant::now();
        filter_batch_indices(
            &values,
            states.iter().map(String::as_str),
            "",
            "attention",
            &mut visible,
        );
        let state_update_ms = started.elapsed().as_millis();
        assert_eq!(visible.len(), 1_000);
        eprintln!(
            "scale-ui rows={rows} filter_ms={filter_ms} selection_all_ms={selection_all_ms} state_update_ms={state_update_ms}"
        );
    }
}
