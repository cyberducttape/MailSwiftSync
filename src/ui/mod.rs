//! Presentation primitives shared by the egui views.

mod account;
mod status;
mod theme;
mod workspace;

#[cfg(test)]
pub(crate) use account::password_reveal_allowed;
pub(crate) use account::{password_visibility_id, render_account};
pub(crate) use status::{
    StatusMessage, StatusSeverity, display_job_state, display_state_key, format_phase_name,
    job_state_badge, needs_operator_review, project_health_state_counts, recommended_next_action,
    status_color, workflow_step_index,
};
#[cfg(test)]
pub(crate) use theme::contrast_ratio;
#[cfg(test)]
pub(crate) use theme::next_ui_scale;
pub(crate) use theme::{AppearancePreferences, ThemeColors};
pub(crate) use workspace::WorkspaceSnapshot;
