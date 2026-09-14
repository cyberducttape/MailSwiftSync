//! Database-backed presentation read model for the workspace views.
//!
//! Rendering must consume these values rather than querying SQLite from an
//! egui view. The controller refreshes the model at a low frequency and
//! invalidates it immediately when the selected project changes.

use crate::core::{self, StateStore};
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Rebuild the project-browser index without cloning project rows. Keeping
/// this policy beside the workspace read model gives the UI a stable, tested
/// searchable view over durable project metadata.
pub(crate) fn filter_project_indices(
    projects: &[core::ProjectListItem],
    query: &str,
    visible_indices: &mut Vec<usize>,
) {
    visible_indices.clear();
    for (index, project) in projects.iter().enumerate() {
        if query.is_empty()
            || contains_ascii_case_insensitive(&project.name, query)
            || contains_ascii_case_insensitive(&project.source_endpoint, query)
            || contains_ascii_case_insensitive(&project.destination_endpoint, query)
        {
            visible_indices.push(index);
        }
    }
}

fn contains_ascii_case_insensitive(value: &str, needle: &str) -> bool {
    value
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// The durable read model rendered by the workspace views. Individual read
/// failures retain the last successful value so a transient SQLite contention
/// event does not blank the operator's view.
#[derive(Default)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) projects: Vec<core::ProjectListItem>,
    pub(crate) projects_revision: u64,
    pub(crate) runs: Vec<core::RunListItem>,
    pub(crate) report: Option<core::ProjectReportSnapshot>,
    pub(crate) project: Option<core::Project>,
    pub(crate) jobs: Vec<core::MailboxJob>,
    snapshot_project_id: Option<String>,
    refreshed_at: Option<Instant>,
    last_successful_refresh: Option<Instant>,
    refresh_error: Option<String>,
}

impl WorkspaceSnapshot {
    pub(crate) fn invalidate(&mut self) {
        self.refreshed_at = None;
    }

    pub(crate) fn stale_notice(&self) -> Option<String> {
        let error = self.refresh_error.as_ref()?;
        let age = self
            .last_successful_refresh
            .map(|at| format!("{} ago", format_refresh_age(at.elapsed())))
            .unwrap_or_else(|| "with no successful refresh yet".into());
        Some(format!(
            "Durable state view is stale ({age}). The last refresh failed: {error}"
        ))
    }

    /// Refresh the UI's durable read model when it is stale or the selected
    /// project changed. Rendering itself never calls SQLite.
    pub(crate) fn refresh(
        &mut self,
        store: &StateStore,
        active_project_id: Option<&str>,
        all_projects_loaded: bool,
    ) {
        let project_id = active_project_id.map(str::to_owned);
        let project_changed = project_id.as_deref() != self.snapshot_project_id.as_deref();
        if !project_changed
            && self
                .refreshed_at
                .is_some_and(|at| at.elapsed() < REFRESH_INTERVAL)
        {
            return;
        }

        self.refreshed_at = Some(Instant::now());
        let mut refresh_errors = Vec::new();
        let project_limit = if all_projects_loaded { usize::MAX } else { 500 };
        match store.recent_projects(project_limit) {
            Ok(value) => {
                if self.projects != value {
                    self.projects = value;
                    self.projects_revision = self.projects_revision.wrapping_add(1);
                }
            }
            Err(error) => refresh_errors.push(format!("project list: {error}")),
        }

        if project_changed {
            self.snapshot_project_id = project_id.clone();
            self.report = None;
            self.runs.clear();
            self.project = None;
            self.jobs.clear();
        }

        let Some(project_id) = project_id else {
            return;
        };

        match store.project(&project_id) {
            Ok(value) => {
                self.project = value;
                if let Some(value) = self.project.as_ref()
                    && !self.projects.iter().any(|item| item.id == value.id)
                {
                    self.projects.push(core::ProjectListItem {
                        id: value.id.clone(),
                        name: value.name.clone(),
                        source_endpoint: value.source_endpoint.clone(),
                        destination_endpoint: value.destination_endpoint.clone(),
                        phase: value.phase,
                    });
                    self.projects_revision = self.projects_revision.wrapping_add(1);
                }
            }
            Err(error) => refresh_errors.push(format!("selected project: {error}")),
        }
        match store.mailboxes(&project_id) {
            Ok(value) => self.jobs = value,
            Err(error) => refresh_errors.push(format!("mailboxes: {error}")),
        }
        match store.project_report_snapshot(&project_id) {
            Ok(Some(value)) => self.report = Some(value),
            Ok(None) => {}
            Err(error) => refresh_errors.push(format!("evidence report: {error}")),
        }
        match store.recent_run_list(&project_id, crate::MAX_ACTIVITY_HISTORY_ROWS) {
            Ok(value) => self.runs = value,
            Err(error) => refresh_errors.push(format!("run history: {error}")),
        }
        if refresh_errors.is_empty() {
            self.last_successful_refresh = Some(Instant::now());
            self.refresh_error = None;
        } else {
            self.refresh_error = Some(refresh_errors.join("; "));
        }
    }
}

fn format_refresh_age(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3_600)
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkspaceSnapshot, filter_project_indices};
    use crate::core::{Phase, ProjectListItem};

    #[test]
    fn project_filter_matches_name_and_endpoints_without_copying_rows() {
        let projects = vec![
            ProjectListItem {
                id: "one".into(),
                name: "Acme migration".into(),
                source_endpoint: "imap.acme.test".into(),
                destination_endpoint: "mail.example.test".into(),
                phase: Phase::Discovery,
            },
            ProjectListItem {
                id: "two".into(),
                name: "Contoso migration".into(),
                source_endpoint: "old.contoso.test".into(),
                destination_endpoint: "mail.example.test".into(),
                phase: Phase::Preflight,
            },
        ];
        let mut visible = vec![99];

        filter_project_indices(&projects, "ACME", &mut visible);
        assert_eq!(visible, vec![0]);
        filter_project_indices(&projects, "CONTOSO.TEST", &mut visible);
        assert_eq!(visible, vec![1]);
        filter_project_indices(&projects, "", &mut visible);
        assert_eq!(visible, vec![0, 1]);
    }

    #[test]
    fn fresh_snapshot_has_no_stale_notice() {
        assert!(WorkspaceSnapshot::default().stale_notice().is_none());
    }
}
