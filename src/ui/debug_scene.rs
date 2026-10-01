//! Debug-build-only startup scenes for visual review.
//!
//! `MAILSWIFTSYNC_DEBUG_VIEW` selects the initial workspace page,
//! `MAILSWIFTSYNC_DEBUG_DIALOG` opens one dialog (including `clear-queue`), and
//! `MAILSWIFTSYNC_DEBUG_DEMO=1` fills the plan and batch queue with
//! placeholder `.example` data, and `MAILSWIFTSYNC_DEBUG_WINDOW=WIDTHxHEIGHT`
//! sets the initial window size. `MAILSWIFTSYNC_DEBUG_PROVIDERS=gmail-m365`
//! selects the Google Workspace → Microsoft 365 presets with placeholder
//! users and no session secrets; `MAILSWIFTSYNC_DEBUG_TELEMETRY=1` seeds
//! synthetic Activity operations telemetry (`gmail-m365-connect` also opens the
//! source card's browser sign-in). Release builds do not compile this module.
use crate::App;
use crate::bulk_import::BulkJob;
#[cfg(debug_assertions)]
use crate::ui::WorkspaceView;

#[cfg(debug_assertions)]
pub(crate) fn apply(app: &mut App) {
    if std::env::var_os("MAILSWIFTSYNC_DEBUG_DEMO").is_some() {
        demo_data(app);
    }
    let providers = std::env::var("MAILSWIFTSYNC_DEBUG_PROVIDERS").unwrap_or_default();
    if providers.starts_with("gmail-m365") {
        providers_demo(app, providers == "gmail-m365-connect");
    }
    if std::env::var_os("MAILSWIFTSYNC_DEBUG_TELEMETRY").is_some() {
        telemetry_demo(app);
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

/// Synthetic operations telemetry over the demo queue: twenty minutes of
/// samples, three transfers in flight, a retry, a cooldown, and a failure.
/// The event channel's sender is leaked so the view renders as running.
pub(crate) fn telemetry_demo(app: &mut App) {
    use crate::progress::TransferProgress;
    use std::time::{Duration, Instant};
    if app.bulk_jobs.is_empty() {
        demo_data(app);
    }
    let now = Instant::now();
    // `Instant` has a platform-defined epoch. On Windows a fresh process can
    // have been alive for less than the requested demo history, and subtract
    // would panic instead of producing a representable instant. Keeping the
    // demo at the current instant is sufficient for the accessibility harness
    // and still renders the full history once the process has enough uptime.
    let start = now.checked_sub(Duration::from_secs(20 * 60)).unwrap_or(now);
    app.run_telemetry.reset(start, app.bulk_jobs.len());
    app.form.profile.batch_concurrency = 4;
    for (index, state) in [(1, "Running"), (2, "Running"), (7, "Running")] {
        app.bulk_jobs[index].state = state.into();
    }
    for (job, finished_at) in [
        ("demo-job-3", 300),
        ("demo-job-4", 520),
        ("demo-job-9", 900),
    ] {
        app.run_telemetry.record_job_finished(
            job,
            "verified",
            "",
            start + Duration::from_secs(finished_at),
        );
    }
    let jobs = [
        ("demo-job-1", 1_900_000_000_u64, 120_000_000_u64),
        ("demo-job-2", 850_000_000, 40_000_000),
        ("demo-job-7", 3_200_000_000, 900_000_000),
    ];
    let mut copied_so_far = [0_u64; 3];
    for step in 0..=40_u64 {
        let at = start + Duration::from_secs(step * 30);
        let wave = 0.75 + 0.25 * ((step as f64) / 4.0).sin();
        for (slot, (job, source_bytes, existing)) in jobs.into_iter().enumerate() {
            if step > 0 {
                copied_so_far[slot] += (source_bytes as f64 * 0.012 * wave) as u64;
            }
            let copied = copied_so_far[slot];
            app.run_telemetry.record_progress(
                job,
                TransferProgress {
                    messages_copied: copied / 48_000,
                    bytes_copied: copied,
                    source_bytes: Some(source_bytes),
                    source_messages: Some(source_bytes / 48_000),
                    destination_bytes_at_start: Some(existing),
                    destination_messages_at_start: Some(existing / 48_000),
                    messages_left: Some((source_bytes.saturating_sub(existing + copied)) / 48_000),
                },
                at,
            );
        }
    }
    app.run_telemetry.record_retry(
        "demo-job-11",
        crate::controller::telemetry::RetryNote {
            attempt: 2,
            retry_at: now + Duration::from_secs(47),
            failure_class: "capacity",
        },
    );
    app.run_telemetry.record_cooldown(
        "imap.source.example:993 → imap.destination.example:993",
        now + Duration::from_secs(95),
    );
    app.run_telemetry.record_job_finished(
        "demo-job-5",
        "failed",
        "[authentication] Destination rejected the credential: AUTHENTICATIONFAILED",
        now.checked_sub(Duration::from_secs(240)).unwrap_or(start),
    );
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::mem::forget(sender);
    app.receiver = Some(receiver);
    app.run_started_at = Some(start);
    app.activity_window_spec = "00:00-23:59".into();
}

/// Google Workspace → Microsoft 365 account cards with no secrets.
pub(crate) fn providers_demo(app: &mut App, connect: bool) {
    app.source_provider = crate::ProviderPreset::GoogleWorkspace;
    app.destination_provider = crate::ProviderPreset::Microsoft365;
    app.apply_provider_preset(true, crate::ProviderPreset::GoogleWorkspace);
    app.apply_provider_preset(false, crate::ProviderPreset::Microsoft365);
    app.form.profile.source_user = "alex@source.example".into();
    app.form.profile.destination_user = "alex@destination.example".into();
    app.form.source_password = crate::credentials::SecretString::default();
    app.form.destination_password = crate::credentials::SecretString::default();
    if connect {
        app.open_card_oauth_connect(true, "google", "alex@source.example");
    }
}

pub(crate) fn demo_data(app: &mut App) {
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
#[cfg(debug_assertions)]
pub(crate) fn window_size() -> Option<[f32; 2]> {
    let value = std::env::var("MAILSWIFTSYNC_DEBUG_WINDOW").ok()?;
    let (width, height) = value.split_once('x')?;
    Some([width.parse().ok()?, height.parse().ok()?])
}
