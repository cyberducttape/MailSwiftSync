//! Presentation primitives shared by the egui views.

mod account;
mod activity;
mod app_state;
mod batch;
mod batch_sheet;
mod engine;
mod output;
mod overview;
mod plan;
mod reports;
mod settings;
mod status;
mod theme;
mod verification;
mod workspace;

#[cfg(test)]
pub(crate) use account::password_reveal_allowed;
pub(crate) use account::{password_visibility_id, render_account};
pub(crate) use output::{
    contains_ascii_case_insensitive, markdown_escape, push_visible_output, redact_secrets,
    truncate_utf8,
};
pub(crate) use status::{
    StatusMessage, StatusSeverity, display_job_state, display_state_key, format_elapsed,
    format_phase_name, job_state_badge, needs_operator_review, project_health_state_counts,
    recommended_next_action, status_color, successful_run_severity, successful_run_status,
    workflow_step_index,
};
#[cfg(test)]
pub(crate) use theme::contrast_ratio;
#[cfg(test)]
pub(crate) use theme::next_ui_scale;
pub(crate) use theme::{AppearancePreferences, ThemeColors};
pub(crate) use workspace::preferred_project_id;
pub(crate) use workspace::{WorkspaceRefreshOptions, WorkspaceSnapshot, WorkspaceView};
mod app;
