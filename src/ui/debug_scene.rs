//! Debug-build-only startup scenes for visual review.
//!
//! `MAILSWIFTSYNC_DEBUG_VIEW` selects the initial workspace page,
//! `MAILSWIFTSYNC_DEBUG_DIALOG` opens one dialog, and
//! `MAILSWIFTSYNC_DEBUG_DEMO=1` fills the plan and batch queue with
//! placeholder `.example` data. Release builds do not compile this module.
use crate::App;
use crate::bulk_import::BulkJob;
use crate::ui::WorkspaceView;

pub(crate) fn apply(app: &mut App) {
    if std::env::var_os("MAILSWIFTSYNC_DEBUG_DEMO").is_some() {
        demo_data(app);
    }
    if let Ok(view) = std::env::var("MAILSWIFTSYNC_DEBUG_VIEW") {
        app.active_view = match view.as_str() {
            "plan" => WorkspaceView::Plan,
            "mailboxes" => WorkspaceView::Mailboxes,
            "activity" => WorkspaceView::Activity,
            "verification" => WorkspaceView::Verification,
            _ => WorkspaceView::Overview,
        };
    }
    if let Ok(dialog) = std::env::var("MAILSWIFTSYNC_DEBUG_DIALOG") {
        match dialog.as_str() {
            "settings" => app.settings_open = true,
            "projects" => app.projects_open = true,
            "keyring" => app.keyring_open = true,
            "engine" => app.engine_open = true,
            "advanced" => app.advanced_open = true,
            "preview" => app.preview = true,
            _ => {}
        }
    }
}

fn demo_data(app: &mut App) {
    let profile = &mut app.form.profile;
    profile.source_host = "imap.source.example".into();
    profile.source_user = "alex@source.example".into();
    profile.destination_host = "imap.destination.example".into();
    profile.destination_user = "alex@destination.example".into();
    let defaults = BulkJob::defaults_from_form(&app.form);
    let states = [
        "queued",
        "ready",
        "running",
        "completed",
        "verified",
        "failed",
        "attention",
        "queued",
        "ready",
        "verified",
        "delta_required",
        "queued",
    ];
    for (index, state) in states.iter().enumerate() {
        let mut form = app.form.clone();
        form.profile.source_user = format!("user{index:02}@source.example");
        form.profile.destination_user = format!("user{index:02}@destination.example");
        app.bulk_jobs.push(BulkJob::from_form_with_defaults(
            format!("Mailbox {:02}", index + 1),
            form,
            (*state).into(),
            std::sync::Arc::clone(&defaults),
        ));
        app.bulk_job_ids.push(format!("demo-job-{index}"));
    }
    app.bulk_jobs_generation = app.bulk_jobs_generation.wrapping_add(1);
}
