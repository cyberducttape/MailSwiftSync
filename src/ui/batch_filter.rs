//! Search and state filtering for the batch mailbox cockpit.

use crate::App;
use crate::ui::{contains_ascii_case_insensitive, display_state_key};

/// Filter the cached row projection without touching the owned queue forms.
/// Keeping this pure makes the large-batch cost measurable independently from
/// egui repainting and prevents accidental per-widget allocations.
#[cfg(test)]
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
    pub(crate) fn rebuild_bulk_search_values(&mut self) {
        self.bulk_state_indices.clear();
        self.bulk_search_values = self
            .bulk_jobs
            .iter()
            .enumerate()
            .map(|(index, job)| {
                self.bulk_state_indices
                    .entry(display_state_key(&job.state))
                    .or_default()
                    .insert(index);
                [
                    job.label.as_str(),
                    job.source_host.as_str(),
                    job.source_user.as_str(),
                    job.destination_host.as_str(),
                    job.destination_user.as_str(),
                ]
                .join(" ")
                .to_ascii_lowercase()
            })
            .collect();
    }

    /// Bring the filtered row list up to date. Returns whether it changed.
    pub(crate) fn refresh_bulk_filter_cache(&mut self) -> bool {
        let raw_search = self.bulk_search.trim().to_owned();
        let cache_is_current = self.bulk_filter_cache_search == raw_search
            && self.bulk_filter_cache_state == self.bulk_state_filter
            && self.bulk_filter_cache_generation == self.bulk_jobs_generation
            && self.bulk_search_values.len() == self.bulk_jobs.len();
        if cache_is_current {
            return false;
        }
        if self.bulk_search_values.len() != self.bulk_jobs.len() {
            self.rebuild_bulk_search_values();
            self.bulk_search_matches_valid = false;
        }
        let normalized_search = raw_search.to_ascii_lowercase();
        self.bulk_visible_indices.clear();
        if !self.bulk_search_matches_valid || self.bulk_filter_cache_search != raw_search {
            self.bulk_search_match_indices.clear();
            if normalized_search.is_empty() {
                self.bulk_search_match_indices
                    .extend(0..self.bulk_jobs.len());
            } else {
                filter_batch_search_indices(
                    &self.bulk_search_values,
                    &normalized_search,
                    &mut self.bulk_search_match_indices,
                );
            }
            self.bulk_search_matches_valid = true;
        }
        // The default mailbox view is already the complete queue. Avoid
        // walking and re-evaluating every row after each durable state update
        // while a large batch is running; egui still virtualizes the table.
        if self.bulk_state_filter.is_empty() || self.bulk_state_filter == "all" {
            self.bulk_visible_indices
                .extend(self.bulk_search_match_indices.iter().copied());
        } else {
            let candidates = self
                .bulk_state_indices
                .iter()
                .filter(|(state, _)| {
                    state.as_str() == self.bulk_state_filter
                        || (self.bulk_state_filter == "delta_required" && state.contains("delta"))
                        || (self.bulk_state_filter == "verification_difference"
                            && state.contains("verification"))
                })
                .flat_map(|(_, indices)| indices.iter().copied())
                .collect::<Vec<_>>();
            let mut candidates = candidates;
            candidates.sort_unstable();
            self.bulk_visible_indices.extend(
                candidates
                    .into_iter()
                    .filter(|index| self.bulk_search_match_indices.binary_search(index).is_ok()),
            );
        }
        self.bulk_filter_cache_search = raw_search;
        self.bulk_filter_cache_state = self.bulk_state_filter.clone();
        self.bulk_filter_cache_generation = self.bulk_jobs_generation;
        true
    }
}

fn filter_batch_search_indices(
    search_values: &[String],
    normalized_search: &str,
    matches: &mut Vec<usize>,
) {
    matches.extend(
        search_values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                (value.contains(normalized_search)
                    || (!normalized_search.is_ascii()
                        && contains_ascii_case_insensitive(value, normalized_search)))
                .then_some(index)
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::filter_batch_indices;
    use std::collections::HashSet;
    use std::path::PathBuf;
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
    fn selection_view_counts_selected_rows_not_all_visible_rows() {
        use crate::bulk_import::BulkJob;
        use crate::{App, Form};

        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-selection-view-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        for index in 0..100 {
            let mut form = Form::default();
            form.profile.source_user = format!("user{index:03}@source.example");
            let state = if index % 10 == 0 {
                "delta_required"
            } else {
                "ready"
            };
            app.bulk_jobs.push(BulkJob::from_form(
                format!("Mailbox {index}"),
                form,
                state.into(),
            ));
            app.bulk_job_ids.push(format!("job-{index}"));
        }
        app.rebuild_bulk_job_index();
        app.bulk_jobs_generation = 1;
        // Rows 0..37 plus one ID no longer in the queue.
        app.bulk_selected_ids = (0..37)
            .map(|index| format!("job-{index}"))
            .chain(["job-gone".to_owned()])
            .collect::<HashSet<_>>();
        // Show rows 0..=9 only.
        app.bulk_search = "user00".into();

        app.refresh_bulk_selection_view();
        let view = &app.bulk_selection_view;
        assert_eq!(view.rows, (0..37).collect::<Vec<_>>());
        assert_eq!(view.visible, 10);
        assert_eq!(view.delta_eligible, 4); // rows 0, 10, 20, 30
        assert_eq!(view.live_eligible, 37);

        app.select_all_bulk_rows();
        app.refresh_bulk_selection_view();
        assert!(app.bulk_selected_ids.is_empty());
        assert_eq!(app.bulk_selection_count(), 100);
        assert!(app.bulk_selection_view.rows.is_empty());
        assert_eq!(app.bulk_selection_view.selected_loaded, 100);
        app.bulk_selected_ids.insert("job-5".into());
        assert_eq!(app.bulk_selection_count(), 99);
        assert!(!app.bulk_is_selected("job-5"));

        drop(app);
        remove_benchmark_state_files(state_path);
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

        let context = eframe::egui::Context::default();
        let first_frame_started = Instant::now();
        let output = context.run_ui(
            eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(1_280.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                eframe::egui::ScrollArea::vertical().show_rows(
                    ui,
                    42.0,
                    rows,
                    |ui, row_range| {
                        for row in row_range {
                            ui.label(format!(
                                "Mailbox {row}: user{row}@source.example → user{row}@destination.example"
                            ));
                        }
                    },
                );
            },
        );
        let first_frame_ms = first_frame_started.elapsed().as_millis();
        assert!(!output.shapes.is_empty());
        eprintln!(
            "scale-ui rows={rows} filter_ms={filter_ms} selection_all_ms={selection_all_ms} state_update_ms={state_update_ms} first_frame_ms={first_frame_ms}"
        );
    }

    #[test]
    #[ignore = "opt-in release full-shell benchmark; run scripts/benchmark-ui-scale.sh"]
    fn full_shell_ui_benchmark() {
        use crate::bulk_import::BulkJob;
        use crate::{App, Form};
        use eframe::App as EframeApp;
        use eframe::egui::{Context, Pos2, RawInput, Rect, vec2};
        let rows = 100_000;
        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-ui-scale-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        let base = Form::default();
        let defaults = BulkJob::defaults_from_form(&base);
        for index in 0..rows {
            let mut form = base.clone();
            form.profile.source_user = format!("user{index}@source.example");
            form.profile.destination_user = format!("user{index}@destination.example");
            app.bulk_jobs.push(BulkJob::from_form_with_defaults(
                format!("Mailbox {index}"),
                form,
                "queued".into(),
                std::sync::Arc::clone(&defaults),
            ));
            app.bulk_job_ids.push(format!("job-{index}"));
        }
        app.rebuild_bulk_job_index();
        app.bulk_jobs_generation = 1;
        app.active_view = crate::ui::WorkspaceView::Mailboxes;

        let context = Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut timed_frame = |app: &mut App| {
            let started = Instant::now();
            let output = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1_280.0, 800.0))),
                    ..Default::default()
                },
                |ui| {
                    EframeApp::logic(app, ui.ctx(), &mut frame);
                    EframeApp::ui(app, ui, &mut frame);
                },
            );
            assert!(!output.shapes.is_empty());
            started.elapsed().as_millis()
        };
        let first_frame_ms = timed_frame(&mut app);
        // Steady-state repaint with every row selected uses compact
        // all-matching state rather than retaining 100k ID strings.
        app.select_all_bulk_rows();
        timed_frame(&mut app);
        let selected_frame_ms = timed_frame(&mut app);
        // One search keystroke: rebuilds the filter cache inside the frame.
        app.bulk_search = "user99999".into();
        let search_frame_ms = timed_frame(&mut app);
        eprintln!(
            "scale-ui-full rows={rows} first_frame_ms={first_frame_ms} selected_frame_ms={selected_frame_ms} search_frame_ms={search_frame_ms}"
        );
        drop(app);
        remove_benchmark_state_files(state_path);
    }

    #[test]
    fn every_workspace_view_renders_without_recursive_page_dispatch() {
        use crate::App;
        use crate::ui::WorkspaceView;
        use eframe::App as EframeApp;
        use eframe::egui::{Context, Pos2, RawInput, Rect, vec2};

        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-overview-regression-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        let context = Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        for view in [
            WorkspaceView::Overview,
            WorkspaceView::Plan,
            WorkspaceView::Mailboxes,
            WorkspaceView::Activity,
            WorkspaceView::Verification,
        ] {
            app.active_view = view;
            let output = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1_280.0, 800.0))),
                    ..Default::default()
                },
                |ui| EframeApp::ui(&mut app, ui, &mut frame),
            );
            assert!(!output.shapes.is_empty());
        }
        drop(app);
        remove_benchmark_state_files(state_path);
    }

    /// A fresh import has no durable project yet, but its rows must still be
    /// renderable and explicitly selectable, or no batch can ever start.
    #[test]
    fn imported_rows_are_selectable_before_first_admission() {
        use crate::bulk_import::{BulkImportResult, BulkJob};
        use crate::{App, Form};

        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-ids-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        let jobs = (0..3)
            .map(|index| {
                let mut form = Form::default();
                form.profile.source_user = format!("user{index}@source.example");
                BulkJob::from_form(format!("Mailbox {index}"), form, "imported".into())
            })
            .collect::<Vec<_>>();
        app.apply_bulk_import_result(Ok(BulkImportResult::Jobs(jobs)));

        assert!(app.bulk_project_id.is_none());
        assert_eq!(app.bulk_job_ids.len(), 3);
        assert_eq!(app.bulk_job_index_by_id.len(), 3);
        app.refresh_bulk_filter_cache();
        assert_eq!(app.bulk_visible_indices, vec![0, 1, 2]);
        let plan = app.current_batch_action_plan(
            crate::controller::BatchExecutionMode::Preflight,
            crate::controller::BulkRetryScope::All,
        );
        assert_eq!(plan.explicit_selection_count, 0);
        app.bulk_selected_ids = app.bulk_job_ids.iter().cloned().collect();
        let plan = app.current_batch_action_plan(
            crate::controller::BatchExecutionMode::Preflight,
            crate::controller::BulkRetryScope::All,
        );
        assert_eq!(plan.explicit_selection_count, 3);
        assert_eq!(plan.eligible_count, 3);
        assert_eq!(plan.hidden_selection_count, 0);

        drop(app);
        remove_benchmark_state_files(state_path);
    }

    /// eframe repaints only on input. While background work is pending the
    /// shell must schedule its own frames, or worker acknowledgements time
    /// out when nobody touches the window.
    #[test]
    fn pending_background_work_schedules_repaints() {
        use crate::App;
        use eframe::App as EframeApp;
        use eframe::egui::{Context, Pos2, RawInput, Rect, ViewportId, vec2};

        let state_path =
            std::env::temp_dir().join(format!("mailswiftsync-repaint-{}.db", uuid::Uuid::new_v4()));
        let mut app = App::from_state_path(Some(&state_path));
        let context = Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut repaint_delay = |app: &mut App| {
            let output = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1_280.0, 800.0))),
                    ..Default::default()
                },
                |ui| {
                    // eframe calls `logic` before `ui` on every frame.
                    EframeApp::logic(app, ui.ctx(), &mut frame);
                    EframeApp::ui(app, ui, &mut frame);
                },
            );
            output.viewport_output[&ViewportId::ROOT].repaint_delay
        };
        // Let startup layout settle, then an idle shell must not spin.
        for _ in 0..3 {
            repaint_delay(&mut app);
        }
        assert!(!app.background_work_pending());

        let (_sender, receiver) = std::sync::mpsc::channel();
        app.bulk_import_receiver = Some(receiver);
        assert!(app.background_work_pending());
        assert!(repaint_delay(&mut app) <= std::time::Duration::from_millis(100));

        drop(app);
        remove_benchmark_state_files(state_path);
    }

    fn remove_benchmark_state_files(state_path: PathBuf) {
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }
}
