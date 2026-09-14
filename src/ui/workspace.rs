//! Database-backed presentation read model for the workspace views.
//!
//! Rendering must consume these values rather than querying SQLite from an
//! egui view. The controller refreshes the model at a low frequency and
//! invalidates it immediately when the selected project changes.

use crate::core::{self, StateStore};
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Refresh the UI's durable read model when it is stale or the selected
/// project changed. Individual read failures retain the last successful value
/// so a transient SQLite contention event does not blank the operator's view.
pub(crate) struct WorkspaceSnapshotRefs<'a> {
    pub(crate) refreshed_at: &'a mut Option<Instant>,
    pub(crate) snapshot_project_id: &'a mut Option<String>,
    pub(crate) projects: &'a mut Vec<core::ProjectListItem>,
    pub(crate) runs: &'a mut Vec<core::RunListItem>,
    pub(crate) report: &'a mut Option<core::ProjectReportSnapshot>,
    pub(crate) project: &'a mut Option<core::Project>,
    pub(crate) jobs: &'a mut Vec<core::MailboxJob>,
}

pub(crate) fn refresh_workspace_snapshot(
    store: &StateStore,
    active_project_id: Option<&str>,
    all_projects_loaded: bool,
    snapshot: WorkspaceSnapshotRefs<'_>,
) {
    let project_id = active_project_id.map(str::to_owned);
    let project_changed = project_id.as_deref() != snapshot.snapshot_project_id.as_deref();
    if !project_changed
        && snapshot
            .refreshed_at
            .is_some_and(|at| at.elapsed() < REFRESH_INTERVAL)
    {
        return;
    }

    *snapshot.refreshed_at = Some(Instant::now());
    let project_limit = if all_projects_loaded { usize::MAX } else { 500 };
    if let Ok(value) = store.recent_projects(project_limit) {
        *snapshot.projects = value;
    }

    if project_changed {
        *snapshot.snapshot_project_id = project_id.clone();
        *snapshot.report = None;
        snapshot.runs.clear();
        *snapshot.project = None;
        snapshot.jobs.clear();
    }

    let Some(project_id) = project_id else {
        return;
    };

    if let Ok(value) = store.project(&project_id) {
        *snapshot.project = value;
        if let Some(value) = snapshot.project.as_ref()
            && !snapshot.projects.iter().any(|item| item.id == value.id)
        {
            snapshot.projects.push(core::ProjectListItem {
                id: value.id.clone(),
                name: value.name.clone(),
                source_endpoint: value.source_endpoint.clone(),
                destination_endpoint: value.destination_endpoint.clone(),
                phase: value.phase,
            });
        }
    }
    if let Ok(value) = store.mailboxes(&project_id) {
        *snapshot.jobs = value;
    }
    if let Ok(Some(value)) = store.project_report_snapshot(&project_id) {
        *snapshot.report = Some(value);
    }
    if let Ok(value) = store.recent_run_list(&project_id, crate::MAX_ACTIVITY_HISTORY_ROWS) {
        *snapshot.runs = value;
    }
}
