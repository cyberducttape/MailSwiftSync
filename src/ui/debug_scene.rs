//! Debug-build-only startup scenes for visual review.
//!
//! `MAILSWIFTSYNC_DEBUG_VIEW` selects the initial workspace page,
//! `MAILSWIFTSYNC_DEBUG_DIALOG` opens one dialog (including `clear-queue`), and
//! `MAILSWIFTSYNC_DEBUG_DEMO=1` fills the plan and batch queue with
//! placeholder `.example` data, and `MAILSWIFTSYNC_DEBUG_WINDOW=WIDTHxHEIGHT`
//! sets the initial window size. `MAILSWIFTSYNC_DEBUG_PROVIDERS=gmail-m365`
//! selects the Google Workspace → Microsoft 365 presets with placeholder
//! users and no session secrets (`gmail-m365-connect` also opens the
//! source card's browser sign-in). Release builds do not compile this module.
use crate::App;
use crate::bulk_import::BulkJob;
use crate::ui::WorkspaceView;

pub(crate) fn apply(app: &mut App) {
    if std::env::var_os("MAILSWIFTSYNC_DEBUG_DEMO").is_some() {
        demo_data(app);
    }
    let providers = std::env::var("MAILSWIFTSYNC_DEBUG_PROVIDERS").unwrap_or_default();
    if providers.starts_with("gmail-m365") {
        app.source_provider = crate::ProviderPreset::GoogleWorkspace;
        app.destination_provider = crate::ProviderPreset::Microsoft365;
        app.apply_provider_preset(true, crate::ProviderPreset::GoogleWorkspace);
        app.apply_provider_preset(false, crate::ProviderPreset::Microsoft365);
        app.form.profile.source_user = "alex@source.example".into();
        app.form.profile.destination_user = "alex@destination.example".into();
        app.form.source_password = crate::credentials::SecretString::default();
        app.form.destination_password = crate::credentials::SecretString::default();
        if providers == "gmail-m365-connect" {
            app.open_card_oauth_connect(true, "google", "alex@source.example");
        }
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
            "clear-queue" => app.bulk_clear_confirm_open = true,
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
    app.rebuild_bulk_job_index();
    app.bulk_jobs_generation = app.bulk_jobs_generation.wrapping_add(1);
    // Masked session placeholders so screenshots show a configured plan
    // rather than "password is required" validation.
    app.form.source_password = String::from("placeholder-secret").into();
    app.form.destination_password = String::from("placeholder-secret").into();
    for id in ["demo-job-1", "demo-job-8"] {
        app.bulk_selected_ids.insert(id.to_owned());
    }
}

/// Initial window size from `MAILSWIFTSYNC_DEBUG_WINDOW`, e.g. `1280x1040`.
pub(crate) fn window_size() -> Option<[f32; 2]> {
    let value = std::env::var("MAILSWIFTSYNC_DEBUG_WINDOW").ok()?;
    let (width, height) = value.split_once('x')?;
    Some([width.parse().ok()?, height.parse().ok()?])
}
