//! Search and state filtering for the batch mailbox cockpit.
//!
//! Filtering runs as SQL against the durable queue (`core::queue`); the
//! view keeps only the matching row IDs and renders rows on demand.

use crate::App;

impl App {
    /// Bring the filtered row index up to date. Returns whether it changed.
    pub(crate) fn refresh_bulk_filter_cache(&mut self) -> bool {
        match self
            .queue
            .refresh_filter(&self.store, &self.bulk_search, &self.bulk_state_filter)
        {
            Ok(changed) => changed,
            Err(error) => {
                self.bulk_message = error;
                false
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    /// Import `rows` mailboxes (label, source user) through the durable
    /// import path, as the GUI does after parsing a file.
    pub(crate) fn import_rows(
        app: &mut crate::App,
        rows: impl IntoIterator<Item = (String, String)>,
    ) {
        use crate::bulk_import::{BulkImportResult, BulkJob};
        let base = crate::Form::default();
        let defaults = BulkJob::defaults_from_form(&base);
        let jobs = rows
            .into_iter()
            .map(|(label, user)| {
                let mut form = base.clone();
                form.profile.source_host = "source.example".into();
                form.profile.destination_host = "destination.example".into();
                form.profile.source_user = user.clone();
                form.profile.destination_user = user.replace("source", "destination");
                BulkJob::from_form_with_defaults(
                    label,
                    form,
                    "imported".into(),
                    std::sync::Arc::clone(&defaults),
                )
            })
            .collect::<Vec<_>>();
        app.apply_bulk_import_result(Ok(BulkImportResult::Jobs(jobs)));
        assert!(app.queue.project_id().is_some(), "{}", app.bulk_message);
    }

    fn visible_labels(app: &mut crate::App) -> Vec<String> {
        app.refresh_bulk_filter_cache();
        let visible = app.queue.visible().to_vec();
        app.queue.load(&app.store, &visible).unwrap();
        visible
            .iter()
            .map(|rowid| app.queue.cached(*rowid).unwrap().label.clone())
            .collect()
    }

    /// A ledger path inside a fresh owner-only directory, so the session is
    /// durable (a ledger directly under a shared temp directory is refused).
    fn temp_state(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        directory.join("state.db")
    }

    #[test]
    fn batch_search_folds_unicode_case() {
        let state_path = temp_state("unicode-search");
        let mut app = crate::App::from_state_path(Some(&state_path));
        import_rows(
            &mut app,
            [
                ("Jürgen Müller", "j.mueller@source.example"),
                ("Straße Archiv", "archiv@source.example"),
                ("Ascii Only", "plain@source.example"),
            ]
            .map(|(label, user)| (label.to_owned(), user.to_owned())),
        );
        for (search, expected) in [
            ("MÜLLER", vec!["Jürgen Müller"]),
            ("strasse", vec!["Straße Archiv"]),
            ("STRAẞE", vec!["Straße Archiv"]),
            ("ASCII", vec!["Ascii Only"]),
        ] {
            app.bulk_search = search.into();
            assert_eq!(visible_labels(&mut app), expected, "search {search:?}");
        }
        app.bulk_search.clear();
        app.bulk_state_filter = "imported".into();
        assert_eq!(visible_labels(&mut app).len(), 3);
        app.bulk_state_filter = "ready".into();
        assert!(visible_labels(&mut app).is_empty());
        drop(app);
        remove_benchmark_state_files(state_path);
    }

    #[test]
    fn selection_view_counts_selected_rows_not_all_visible_rows() {
        let state_path = temp_state("selection-view");
        let mut app = crate::App::from_state_path(Some(&state_path));
        import_rows(
            &mut app,
            (0..100).map(|index| {
                (
                    format!("Mailbox {index}"),
                    format!("user{index:03}@source.example"),
                )
            }),
        );
        let ids = app
            .store
            .mailbox_ids(app.queue.project_id().unwrap())
            .unwrap();
        for (index, id) in ids.iter().enumerate() {
            let state = if index % 10 == 0 {
                "delta_required"
            } else {
                "ready"
            };
            app.store.force_mailbox_state(id, state).unwrap();
        }
        app.mark_bulk_jobs_changed();
        // Rows 0..37 plus one ID no longer in the queue.
        app.bulk_selected_ids = ids[..37]
            .iter()
            .cloned()
            .chain(["job-gone".to_owned()])
            .collect();
        // Show rows 0..=9 only.
        app.bulk_search = "user00".into();

        app.refresh_bulk_selection_view();
        let view = &app.bulk_selection_view;
        assert_eq!(view.rows.len(), 37);
        assert_eq!(view.selected_loaded, 37);
        assert_eq!(view.visible, 10);
        assert_eq!(view.delta_eligible, 4); // rows 0, 10, 20, 30
        assert_eq!(view.live_eligible, 37);
        assert_eq!(view.ready, 33);

        app.select_all_bulk_rows();
        app.refresh_bulk_selection_view();
        assert!(app.bulk_selected_ids.is_empty());
        assert_eq!(app.bulk_selection_count(), 100);
        assert!(app.bulk_selection_view.rows.is_empty());
        assert_eq!(app.bulk_selection_view.selected_loaded, 100);
        app.bulk_selected_ids.insert(ids[5].clone());
        assert_eq!(app.bulk_selection_count(), 99);
        assert!(!app.bulk_is_selected(&ids[5]));

        drop(app);
        remove_benchmark_state_files(state_path);
    }

    /// A fresh import is durable at once: its rows render and are
    /// explicitly selectable before any preflight has run.
    #[test]
    fn imported_rows_are_durable_and_selectable_before_first_admission() {
        let state_path = temp_state("import-ids");
        let mut app = crate::App::from_state_path(Some(&state_path));
        import_rows(
            &mut app,
            (0..3).map(|index| {
                (
                    format!("Mailbox {index}"),
                    format!("user{index}@source.example"),
                )
            }),
        );
        let project_id = app.queue.project_id().unwrap().to_owned();
        assert_eq!(app.store.queue_len(&project_id).unwrap(), 3);
        assert_eq!(
            visible_labels(&mut app),
            ["Mailbox 0", "Mailbox 1", "Mailbox 2"]
        );
        let plan = app.current_batch_action_plan(
            crate::controller::BatchExecutionMode::Preflight,
            crate::controller::BulkRetryScope::All,
        );
        assert_eq!(plan.explicit_selection_count, 0);
        app.bulk_selected_ids = app
            .store
            .mailbox_ids(&project_id)
            .unwrap()
            .into_iter()
            .collect();
        let plan = app.current_batch_action_plan(
            crate::controller::BatchExecutionMode::Preflight,
            crate::controller::BulkRetryScope::All,
        );
        assert_eq!(plan.explicit_selection_count, 3);
        assert_eq!(plan.eligible_count, 3);
        assert_eq!(plan.hidden_selection_count, 0);
        drop(app);

        // A restart restores the same queue from the ledger.
        let mut app = crate::App::from_state_path(Some(&state_path));
        assert_eq!(app.queue.project_id(), Some(project_id.as_str()));
        assert_eq!(
            visible_labels(&mut app),
            ["Mailbox 0", "Mailbox 1", "Mailbox 2"]
        );
        drop(app);
        remove_benchmark_state_files(state_path);
    }

    /// End to end through the GUI start path: a durable import with session
    /// passwords, a batch preflight that the scheduler executes from the
    /// ledger (on its own read-only connection) against a fake engine, and
    /// durable ready states the restored queue then presents.
    #[cfg(unix)]
    #[test]
    fn batch_preflight_runs_from_the_durable_queue() {
        use crate::bulk_import::{BulkImportResult, BulkJob};
        use std::os::unix::fs::PermissionsExt;
        let state_path = temp_state("queue-preflight");
        let engine = state_path.parent().unwrap().join("fake-imapsync");
        std::fs::write(&engine, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut app = crate::App::from_state_path(Some(&state_path));
        app.form.profile.engine = crate::core::Engine::ImapSync;
        app.form.profile.imapsync_path = engine.to_string_lossy().into_owned();
        let jobs = (0..3)
            .map(|index| {
                let mut form = app.form.clone_without_credentials();
                form.profile.source_host = "source.example".into();
                form.profile.destination_host = "destination.example".into();
                form.profile.source_user = format!("user{index}@source.example");
                form.profile.destination_user = format!("user{index}@destination.example");
                form.source_password = String::from("source-secret").into();
                form.destination_password = String::from("destination-secret").into();
                BulkJob::from_form(format!("Mailbox {index}"), form, "imported".into())
            })
            .collect::<Vec<_>>();
        app.apply_bulk_import_result(Ok(BulkImportResult::Jobs(jobs)));
        let project_id = app.queue.project_id().unwrap().to_owned();
        assert_eq!(app.queue.session_secrets.len(), 3);
        app.select_all_bulk_rows();
        app.bulk_mode = crate::controller::BatchExecutionMode::Preflight;
        app.bulk_retry_scope = crate::controller::BulkRetryScope::All;
        app.start_bulk();
        assert!(app.receiver.is_some(), "{}", app.bulk_message);
        crate::headless::wait_for_headless_controller(&mut app).unwrap();
        let mut states = Vec::new();
        app.store
            .queue_durable_scan(&project_id, |row| states.push(row.durable_state))
            .unwrap();
        assert_eq!(states, ["ready", "ready", "ready"], "{}", app.bulk_message);
        // A successful dry run records each row's credential binding for the
        // live gate, in this session only.
        assert_eq!(app.queue.preflight_credentials.len(), 3);
        assert_eq!(app.bulk_queue_summary().ready, 3);
        drop(app);

        let mut app = crate::App::from_state_path(Some(&state_path));
        assert_eq!(app.queue.project_id(), Some(project_id.as_str()));
        assert_eq!(app.bulk_queue_summary().ready, 3);
        assert!(app.queue.session_secrets.is_empty());
        drop(app);
        remove_benchmark_state_files(state_path);
    }

    /// The release benchmark seeds a 100,000-row durable queue and measures
    /// SQL filtering, select-all accounting, a 1,000-row state change, and
    /// the first Mailboxes frame through the real App.
    #[test]
    #[ignore = "opt-in release UI scale benchmark; run scripts/benchmark-ui-scale.sh"]
    fn scale_ui_benchmark() {
        use eframe::App as EframeApp;
        use eframe::egui::{Context, Pos2, RawInput, Rect, vec2};
        let rows = 100_000;
        let state_path = temp_state("ui-scale");
        let mut app = crate::App::from_state_path(Some(&state_path));
        // Parsing is measured by the import benchmark; this is the durable
        // write of the parsed rows as a new batch queue.
        let started = Instant::now();
        import_rows(
            &mut app,
            (0..rows).map(|index| {
                (
                    format!("Mailbox {index}"),
                    format!("user{index}@source.example"),
                )
            }),
        );
        let persist_ms = started.elapsed().as_millis();

        app.bulk_search = "user50000@".into();
        let started = Instant::now();
        app.refresh_bulk_filter_cache();
        let filter_ms = started.elapsed().as_millis();
        assert_eq!(app.queue.visible().len(), 1);
        app.bulk_search.clear();

        let started = Instant::now();
        app.select_all_bulk_rows();
        app.refresh_bulk_selection_view();
        let selection_all_ms = started.elapsed().as_millis();
        assert_eq!(app.bulk_selection_view.selected_loaded, rows);

        let ids = app
            .store
            .mailbox_ids(app.queue.project_id().unwrap())
            .unwrap();
        for id in ids.iter().take(1_000) {
            app.store.force_mailbox_state(id, "attention").unwrap();
        }
        app.bulk_state_filter = "attention".into();
        let started = Instant::now();
        app.mark_bulk_jobs_changed();
        app.refresh_bulk_filter_cache();
        let state_update_ms = started.elapsed().as_millis();
        assert_eq!(app.queue.visible().len(), 1_000);
        app.bulk_state_filter = "all".into();

        app.active_view = crate::ui::WorkspaceView::Mailboxes;
        let context = Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let first_frame_started = Instant::now();
        let output = context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1_280.0, 800.0))),
                ..Default::default()
            },
            |ui| EframeApp::ui(&mut app, ui, &mut frame),
        );
        let first_frame_ms = first_frame_started.elapsed().as_millis();
        assert!(!output.shapes.is_empty());
        eprintln!(
            "scale-ui rows={rows} filter_ms={filter_ms} selection_all_ms={selection_all_ms} state_update_ms={state_update_ms} first_frame_ms={first_frame_ms} persist_ms={persist_ms}"
        );
        drop(app);
        remove_benchmark_state_files(state_path);
    }

    #[test]
    #[ignore = "opt-in release full-shell benchmark; run scripts/benchmark-ui-scale.sh"]
    fn full_shell_ui_benchmark() {
        use eframe::App as EframeApp;
        use eframe::egui::{Context, Pos2, RawInput, Rect, vec2};
        let rows = 100_000;
        let state_path = temp_state("ui-shell-scale");
        let mut app = crate::App::from_state_path(Some(&state_path));
        import_rows(
            &mut app,
            (0..rows).map(|index| {
                (
                    format!("Mailbox {index}"),
                    format!("user{index}@source.example"),
                )
            }),
        );
        app.active_view = crate::ui::WorkspaceView::Mailboxes;

        let context = Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut timed_frame = |app: &mut crate::App| {
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
        // One search keystroke: re-filters in SQL inside the frame.
        app.bulk_search = "user99999@".into();
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
            WorkspaceView::Recovery,
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
        if let Some(directory) = state_path.parent()
            && directory.starts_with(std::env::temp_dir())
            && directory != std::env::temp_dir()
        {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}
