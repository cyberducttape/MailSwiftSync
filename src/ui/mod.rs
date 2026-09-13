//! Presentation primitives shared by the egui views.

mod status;
mod theme;

#[cfg(test)]
pub(crate) use status::{StatusSeverity, status_severity};
pub(crate) use status::{
    display_job_state, display_state_key, format_phase_name, job_state_badge,
    needs_operator_review, project_health_state_counts, recommended_next_action, status_color,
    workflow_step_index,
};
pub(crate) use theme::ThemeColors;
#[cfg(test)]
pub(crate) use theme::contrast_ratio;
