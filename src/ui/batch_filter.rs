//! Search and state filtering for the batch mailbox cockpit.

use crate::App;
use crate::bulk_import::BulkJob;
use crate::ui::{contains_ascii_case_insensitive, display_state_key};

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
        for (index, job) in self.bulk_jobs.iter().enumerate() {
            let state = display_state_key(&job.state);
            let state_matches = self.bulk_state_filter.is_empty()
                || self.bulk_state_filter == "all"
                || state == self.bulk_state_filter
                || (self.bulk_state_filter == "delta_required" && state.contains("delta"))
                || (self.bulk_state_filter == "verification_difference"
                    && state.contains("verification"));
            let search_matches = normalized_search.is_empty()
                || self
                    .bulk_search_values
                    .get(index)
                    .is_some_and(|value| value.contains(&normalized_search));
            if state_matches && search_matches {
                self.bulk_visible_indices.push(index);
            }
        }
        self.bulk_filter_cache_search = raw_search;
        self.bulk_filter_cache_state = self.bulk_state_filter.clone();
        self.bulk_filter_cache_generation = self.bulk_jobs_generation;
    }
}
