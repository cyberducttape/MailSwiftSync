//! Database-backed presentation read model for the workspace views.
//!
//! Rendering must consume these values rather than querying SQLite from an
//! egui view. The controller refreshes the model at a low frequency and
//! invalidates it immediately when the selected project changes.

use crate::core::{self, StateStore};
use crate::ui::contains_ascii_case_insensitive;
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const MAILBOX_PAGE_SIZE: u32 = 200;

pub(crate) struct WorkspaceRefreshOptions<'a> {
    pub(crate) active_project_id: Option<&'a str>,
    pub(crate) all_projects_loaded: bool,
    pub(crate) mailbox_offset: u32,
    pub(crate) verification_offset: u32,
    pub(crate) load_report: bool,
    pub(crate) load_runs: bool,
}

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

/// The durable read model rendered by the workspace views. Individual read
/// failures retain the last successful value so a transient SQLite contention
/// event does not blank the operator's view.
#[derive(Default)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) projects: Vec<core::ProjectListItem>,
    pub(crate) projects_revision: u64,
    pub(crate) runs: Vec<core::RunListItem>,
    pub(crate) verification_rows: Vec<core::ReportMailboxSnapshot>,
    pub(crate) verification_loaded: bool,
    verification_offset: u32,
    pub(crate) project: Option<core::Project>,
    pub(crate) jobs: Vec<core::MailboxJob>,
    pub(crate) mailbox_counts: core::MailboxStateCounts,
    snapshot_project_id: Option<String>,
    durable_revision: Option<i64>,
    project_revision: Option<i64>,
    mailbox_offset: Option<u32>,
    runs_revision: Option<i64>,
    refreshed_at: Option<Instant>,
    last_successful_refresh: Option<Instant>,
    refresh_error: Option<String>,
}

impl WorkspaceSnapshot {
    pub(crate) fn invalidate(&mut self) {
        self.refreshed_at = None;
        self.verification_rows.clear();
        self.verification_loaded = false;
        self.verification_offset = 0;
        self.durable_revision = None;
        self.project_revision = None;
        self.mailbox_offset = None;
        self.runs_revision = None;
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

    /// Whether any part of the cached durable view failed to refresh. A stale
    /// projection may remain visible for continuity, but callers must not
    /// treat it as current state for actions that can mutate a migration.
    pub(crate) fn is_stale(&self) -> bool {
        self.refresh_error.is_some()
    }

    /// Refresh the UI's durable read model when it is stale or the selected
    /// project changed. Rendering itself never calls SQLite.
    pub(crate) fn refresh(&mut self, store: &StateStore, options: WorkspaceRefreshOptions<'_>) {
        let project_id = options.active_project_id.map(str::to_owned);
        let project_changed = project_id.as_deref() != self.snapshot_project_id.as_deref();
        let report_needs_load = options.load_report
            && (!self.verification_loaded
                || self.verification_offset != options.verification_offset);
        let runs_need_load = options.load_runs && self.runs_revision != self.project_revision;
        if !project_changed
            && !report_needs_load
            && self
                .refreshed_at
                .is_some_and(|at| at.elapsed() < REFRESH_INTERVAL)
        {
            return;
        }

        self.refreshed_at = Some(Instant::now());
        let mut refresh_errors = Vec::new();
        let observed_revision = match store.read_model_revision() {
            Ok(revision) => Some(revision),
            Err(error) => {
                refresh_errors.push(format!("read-model revision: {error}"));
                None
            }
        };
        let observed_project_revision =
            project_id
                .as_deref()
                .and_then(|id| match store.project_read_model_revision(id) {
                    Ok(revision) => Some(revision),
                    Err(error) => {
                        refresh_errors.push(format!("project read-model revision: {error}"));
                        None
                    }
                });
        if !project_changed
            && !report_needs_load
            && !runs_need_load
            && observed_revision.is_some()
            && observed_revision == self.durable_revision
            && observed_project_revision == self.project_revision
            && refresh_errors.is_empty()
        {
            self.refresh_error = None;
            self.last_successful_refresh = Some(Instant::now());
            return;
        }
        let project_limit = if options.all_projects_loaded {
            usize::MAX
        } else {
            500
        };
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
            self.verification_rows.clear();
            self.verification_loaded = false;
            self.verification_offset = 0;
            self.runs.clear();
            self.runs_revision = None;
            self.project = None;
            self.jobs.clear();
        }

        let Some(project_id) = project_id else {
            if refresh_errors.is_empty() {
                self.durable_revision = observed_revision;
                self.last_successful_refresh = Some(Instant::now());
                self.refresh_error = None;
            } else {
                self.refresh_error = Some(refresh_errors.join("; "));
            }
            return;
        };

        let project_data_changed = project_changed
            || observed_project_revision != self.project_revision
            || self.project.is_none()
            || self.mailbox_offset != Some(options.mailbox_offset);
        if project_data_changed {
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
            match store.mailbox_page(&project_id, options.mailbox_offset, MAILBOX_PAGE_SIZE) {
                Ok(value) => {
                    self.jobs = value;
                    self.mailbox_offset = Some(options.mailbox_offset);
                }
                Err(error) => refresh_errors.push(format!("mailboxes: {error}")),
            }
            match store.mailbox_state_counts(&project_id) {
                Ok(value) => self.mailbox_counts = value,
                Err(error) => refresh_errors.push(format!("mailbox counts: {error}")),
            }
        }
        if options.load_report {
            match store.verification_rows(
                &project_id,
                options.verification_offset,
                MAILBOX_PAGE_SIZE,
            ) {
                Ok(value) => {
                    self.verification_rows = value;
                    self.verification_loaded = true;
                    self.verification_offset = options.verification_offset;
                }
                Err(error) => refresh_errors.push(format!("verification rows: {error}")),
            }
        }
        if options.load_runs {
            match store.recent_run_list(&project_id, crate::MAX_ACTIVITY_HISTORY_ROWS) {
                Ok(value) => self.runs = value,
                Err(error) => refresh_errors.push(format!("run history: {error}")),
            }
            if refresh_errors.is_empty() {
                self.runs_revision = observed_project_revision;
            }
        }
        if refresh_errors.is_empty() {
            self.durable_revision = observed_revision;
            self.project_revision = observed_project_revision;
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
        let snapshot = WorkspaceSnapshot::default();
        assert!(!snapshot.is_stale());
        assert!(snapshot.stale_notice().is_none());
    }

    #[test]
    fn failed_refresh_is_explicitly_stale_even_when_old_data_is_retained() {
        let snapshot = WorkspaceSnapshot {
            refresh_error: Some("database is busy".into()),
            ..WorkspaceSnapshot::default()
        };
        assert!(snapshot.is_stale());
        let notice = snapshot.stale_notice().unwrap();
        assert!(notice.contains("no successful refresh yet"));
        assert!(notice.contains("database is busy"));
    }
}
