//! Database-backed presentation read model for the workspace views.
//!
//! Rendering must consume these values rather than querying SQLite from an
//! egui view. The controller refreshes the model at a low frequency and
//! invalidates it immediately when the selected project changes.

use crate::core::{self, StateStore};
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

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
}

impl WorkspaceSnapshot {
    pub(crate) fn invalidate(&mut self) {
        self.refreshed_at = None;
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
        let project_limit = if all_projects_loaded { usize::MAX } else { 500 };
        if let Ok(value) = store.recent_projects(project_limit)
            && self.projects != value
        {
            self.projects = value;
            self.projects_revision = self.projects_revision.wrapping_add(1);
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

        if let Ok(value) = store.project(&project_id) {
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
        if let Ok(value) = store.mailboxes(&project_id) {
            self.jobs = value;
        }
        if let Ok(Some(value)) = store.project_report_snapshot(&project_id) {
            self.report = Some(value);
        }
        if let Ok(value) = store.recent_run_list(&project_id, crate::MAX_ACTIVITY_HISTORY_ROWS) {
            self.runs = value;
        }
    }
}
