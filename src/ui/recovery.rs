//! Recovery workspace: every mailbox that needs an operator decision,
//! grouped by state and durable attention reason, with the reason's
//! recommended action and checklist, the affected mailboxes and their latest
//! run and transfer attempt, and hand-offs into the existing safe paths
//! (explicit selection on the Mailboxes page, evidence review). Nothing here
//! starts work by itself.

use crate::App;
use crate::core::{AttentionReason, RecoveryGroup, RecoveryRow};
use crate::ui::WorkspaceView;
use eframe::egui::{self, RichText};
use std::time::{Duration, Instant};

const RECOVERY_PAGE_ROWS: u32 = 25;
/// How long recovery groups are reused before being read again.
const RECOVERY_REFRESH: Duration = Duration::from_secs(2);

/// One recovery group's identity.
type GroupKey = (String, Option<AttentionReason>);

#[derive(Default)]
pub(crate) struct RecoveryState {
    groups: Vec<RecoveryGroup>,
    loaded: Option<(String, Instant)>,
    expanded: Option<GroupKey>,
    rows: Vec<RecoveryRow>,
    /// Keyset cursor of the page shown, and of the pages before it.
    page_after: Option<i64>,
    cursors: Vec<Option<i64>>,
    error: Option<String>,
}

impl RecoveryState {
    /// Read groups again on the next frame.
    pub(crate) fn invalidate(&mut self) {
        self.loaded = None;
    }
}

/// What an operator should do for a group, beyond its reason's checklist.
fn recommended_action(group: &RecoveryGroup) -> &'static str {
    match (group.state.as_str(), group.reason) {
        ("delta_required", _) => "ui.recovery-delta-action",
        ("cancelled", None) => "ui.recovery-cancelled-action",
        (_, None) => "ui.recovery-unclassified-action",
        _ => "",
    }
}

/// What MailSwiftSync already did about a group's failure before handing it
/// to the operator, by its durable reason.
fn already_tried(group: &RecoveryGroup) -> &'static str {
    match group.reason {
        Some(
            AttentionReason::TransportFailed
            | AttentionReason::CapacityLimited
            | AttentionReason::Interrupted,
        ) => "ui.recovery-tried-transient",
        Some(
            AttentionReason::AuthenticationFailed
            | AttentionReason::ConfigurationInvalid
            | AttentionReason::PolicyBlocked
            | AttentionReason::MessageRejected,
        ) => "ui.recovery-tried-not-retried",
        Some(
            AttentionReason::VerificationDifference
            | AttentionReason::VerificationIncomplete
            | AttentionReason::VerificationLimitExceeded,
        ) => "ui.recovery-tried-verification",
        Some(AttentionReason::ProcessIdentityUnverified) => "ui.recovery-tried-process",
        Some(AttentionReason::Unknown) | None => match group.state.as_str() {
            "delta_required" => "ui.recovery-tried-delta",
            "cancelled" => "ui.recovery-tried-cancelled",
            _ => "ui.recovery-tried-unclassified",
        },
    }
}

fn reviews_evidence(group: &RecoveryGroup) -> bool {
    group.state == "verification_difference"
        || matches!(
            group.reason,
            Some(
                AttentionReason::VerificationDifference
                    | AttentionReason::VerificationIncomplete
                    | AttentionReason::VerificationLimitExceeded
            )
        )
}

/// Locale keys for why a group stopped and the follow-up button, plus the
/// view that button opens.
fn remediation(group: &RecoveryGroup) -> (&'static str, &'static str, WorkspaceView) {
    match group.reason {
        Some(AttentionReason::AuthenticationFailed) => (
            "ui.recovery-why-authentication",
            "ui.recovery-action-reconnect",
            WorkspaceView::Plan,
        ),
        Some(AttentionReason::CapacityLimited) => (
            "ui.recovery-why-capacity",
            "ui.recovery-action-queue-policy",
            WorkspaceView::Mailboxes,
        ),
        Some(AttentionReason::VerificationDifference | AttentionReason::VerificationIncomplete) => {
            (
                "ui.recovery-why-verification",
                "ui.recovery-action-verification",
                WorkspaceView::Verification,
            )
        }
        Some(AttentionReason::VerificationLimitExceeded) => (
            "ui.recovery-why-verification-limit",
            "ui.recovery-action-verification",
            WorkspaceView::Verification,
        ),
        Some(AttentionReason::ConfigurationInvalid | AttentionReason::PolicyBlocked) => (
            "ui.recovery-why-configuration",
            "ui.recovery-action-plan",
            WorkspaceView::Plan,
        ),
        Some(AttentionReason::TransportFailed) => (
            "ui.recovery-why-transport",
            "ui.recovery-action-mailboxes",
            WorkspaceView::Mailboxes,
        ),
        Some(AttentionReason::Interrupted | AttentionReason::ProcessIdentityUnverified) => (
            "ui.recovery-why-interrupted",
            "ui.recovery-action-recovery-mailboxes",
            WorkspaceView::Mailboxes,
        ),
        Some(AttentionReason::MessageRejected) => (
            "ui.recovery-why-message-rejected",
            "ui.recovery-action-mailboxes",
            WorkspaceView::Mailboxes,
        ),
        // Unclassified groups are explained by their durable state, the
        // same way `already_tried` words them.
        Some(AttentionReason::Unknown) | None => (
            match group.state.as_str() {
                "delta_required" => "ui.recovery-why-delta",
                "cancelled" => "ui.recovery-why-cancelled",
                _ => "ui.recovery-why-unclassified",
            },
            "ui.recovery-action-mailboxes",
            WorkspaceView::Mailboxes,
        ),
    }
}
fn progress_checkpoint_label(detail: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(detail) else {
        return "Last durable transfer checkpoint is available".to_owned();
    };
    let attempt = value.get("attempt").and_then(serde_json::Value::as_u64);
    let progress = value.get("progress");
    let messages = progress
        .and_then(|value| value.get("messages_copied"))
        .and_then(serde_json::Value::as_u64);
    let bytes = progress
        .and_then(|value| value.get("bytes_copied"))
        .and_then(serde_json::Value::as_u64);
    match (attempt, messages, bytes) {
        (Some(attempt), Some(messages), Some(bytes)) => {
            format!(
                "Last durable transfer checkpoint: attempt {attempt}, {messages} messages, {bytes} bytes"
            )
        }
        _ => "Last durable transfer checkpoint is available".to_owned(),
    }
}

impl App {
    fn refresh_recovery(&mut self, project_id: &str) {
        let fresh =
            self.recovery.loaded.as_ref().is_some_and(|(loaded, at)| {
                loaded == project_id && at.elapsed() < RECOVERY_REFRESH
            });
        if fresh {
            return;
        }
        match self.store.recovery_groups(project_id) {
            Ok(groups) => {
                self.recovery.groups = groups;
                self.recovery.error = None;
            }
            Err(error) => {
                self.recovery.error = Some(format!("Could not read recovery state: {error}"));
                self.recovery.loaded = Some((project_id.to_owned(), Instant::now()));
                return;
            }
        }
        let still_present = self
            .recovery
            .expanded
            .as_ref()
            .is_some_and(|(state, reason)| {
                self.recovery
                    .groups
                    .iter()
                    .any(|group| &group.state == state && &group.reason == reason)
            });
        if !still_present {
            self.recovery.expanded = None;
            self.recovery.rows.clear();
            self.recovery.cursors.clear();
            self.recovery.page_after = None;
        } else {
            self.load_recovery_page(project_id, self.recovery.page_after);
        }
        self.recovery.loaded = Some((project_id.to_owned(), Instant::now()));
    }

    fn load_recovery_page(&mut self, project_id: &str, after: Option<i64>) {
        let Some((state, reason)) = self.recovery.expanded.clone() else {
            return;
        };
        self.recovery.page_after = after;
        match self
            .store
            .recovery_rows(project_id, &state, reason, after, RECOVERY_PAGE_ROWS)
        {
            Ok(rows) => self.recovery.rows = rows,
            Err(error) => {
                self.recovery.error = Some(format!("Could not read recovery state: {error}"));
            }
        }
    }

    /// Hand a group to the Mailboxes page as an explicit selection; the
    /// operator still chooses and confirms the run there.
    fn select_recovery_group(&mut self, project_id: &str, group: &RecoveryGroup) {
        match self
            .store
            .recovery_group_ids(project_id, &group.state, group.reason)
        {
            Ok(ids) => {
                self.bulk_all_selected = false;
                self.bulk_selected_ids = ids.into_iter().collect();
                self.bulk_selection_view_dirty = true;
                self.bulk_state_filter = "all".into();
                self.bulk_search.clear();
                self.bulk_inspector_open = true;
                self.active_view = WorkspaceView::Mailboxes;
            }
            Err(error) => {
                self.recovery.error = Some(format!("Could not read recovery state: {error}"));
            }
        }
    }

    pub(crate) fn recovery_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        crate::ui::page_header(
            ui,
            self.language.message("ui.recovery"),
            self.language.message("ui.recovery-subtitle"),
        );
        crate::ui::card(ui, |ui| {
            ui.label(
                RichText::new(self.language.message("ui.recovery-resume-boundary"))
                    .color(colors.info),
            );
        });
        ui.add_space(8.0);
        if self.process_review_required {
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(self.language.message("ui.recovery-process-review"))
                        .color(colors.danger),
                );
            });
            ui.add_space(8.0);
        }
        let Some(project_id) = self.active_project_id().map(str::to_owned) else {
            ui.label(
                RichText::new(self.language.message("ui.recovery-no-project"))
                    .color(colors.text_secondary),
            );
            return;
        };
        self.refresh_recovery(&project_id);
        if let Some(error) = &self.recovery.error {
            ui.colored_label(colors.danger, error);
            if ui
                .button(self.language.message("ui.retry-evidence-read"))
                .clicked()
            {
                self.recovery.invalidate();
            }
            return;
        }
        if self.recovery.groups.is_empty() {
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(self.language.message("ui.recovery-nothing"))
                        .color(colors.success),
                );
                if ui
                    .button(self.language.message("ui.review-verification"))
                    .clicked()
                {
                    self.active_view = WorkspaceView::Verification;
                }
            });
            return;
        }
        let selectable = self.queue.project_id() == Some(project_id.as_str());
        let groups = self.recovery.groups.clone();
        let mut toggle = None;
        let mut select = None;
        let mut review = None;
        let mut remediate = None;
        let mut retry = None;
        for group in &groups {
            let key: GroupKey = (group.state.clone(), group.reason);
            let expanded = self.recovery.expanded.as_ref() == Some(&key);
            crate::ui::card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let state = self
                        .language
                        .text(crate::ui::display_job_state(&group.state));
                    let title = match group.reason {
                        Some(reason) => format!("{state} · {}", self.language.text(reason.label())),
                        None => state.to_owned(),
                    };
                    ui.heading(title);
                    crate::ui::pill(
                        ui,
                        &self
                            .language
                            .message("ui.recovery-mailboxes-count")
                            .replace("{}", &group.count.to_string()),
                        colors.warning,
                    );
                });
                let (consequence, remediation_label, remediation_view) = remediation(group);
                crate::ui::section_label(ui, self.language.message("ui.recovery-why"));
                ui.label(self.language.message(consequence));
                crate::ui::section_label(ui, self.language.message("ui.recovery-tried"));
                ui.label(self.language.message(already_tried(group)));
                crate::ui::section_label(ui, self.language.message("ui.recovery-what-to-do"));
                if let Some(reason) = group.reason {
                    ui.label(self.language.text(reason.recommended_action()));
                }
                let extra = recommended_action(group);
                if !extra.is_empty() {
                    ui.label(self.language.message(extra));
                }
                if let Some(guidance) = group
                    .reason
                    .and_then(
                        crate::core::recovery_dashboard::InterruptionReason::from_attention_reason,
                    )
                    .map(crate::core::recovery_dashboard::RecoveryPlanner::generate_guidance)
                {
                    ui.add_space(4.0);
                    crate::ui::section_label(ui, self.language.message("ui.recovery-checklist"));
                    for step in guidance {
                        ui.label(format!("• {}", self.language.text(step)));
                    }
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let toggle_label = if expanded {
                        self.language.message("ui.recovery-hide")
                    } else {
                        self.language.message("ui.recovery-show")
                    };
                    if ui.button(toggle_label).clicked() {
                        toggle = Some(key.clone());
                    }
                    let select_button = ui
                        .add_enabled(
                            selectable && !self.running(),
                            egui::Button::new(self.language.message("ui.recovery-select")),
                        )
                        .on_disabled_hover_text(self.language.message("ui.recovery-queue-only"));
                    if select_button.clicked() {
                        select = Some(group.clone());
                    }
                    let retry_button = ui
                        .add_enabled(
                            selectable && !self.running(),
                            egui::Button::new(
                                self.language
                                    .message(if group.state == "delta_required" {
                                        "ui.recovery-run-delta"
                                    } else {
                                        "ui.recovery-retry-preflight"
                                    })
                                    .replace("{}", &group.count.to_string()),
                            ),
                        )
                        .on_hover_text(self.language.message(if group.state == "delta_required" {
                            "ui.recovery-run-delta-hint"
                        } else {
                            "ui.recovery-retry-hint"
                        }))
                        .on_disabled_hover_text(self.language.message("ui.recovery-queue-only"));
                    if retry_button.clicked() {
                        retry = Some(group.clone());
                    }
                    if ui
                        .button(self.language.message(remediation_label))
                        .clicked()
                    {
                        remediate = Some(remediation_view);
                    }
                    if reviews_evidence(group)
                        && ui
                            .button(self.language.message("ui.recovery-review-evidence"))
                            .clicked()
                    {
                        review = Some(group.reason);
                    }
                });
                if expanded {
                    ui.separator();
                    self.recovery_rows_table(ui, &project_id);
                }
            });
            ui.add_space(8.0);
        }
        if let Some(key) = toggle {
            if self.recovery.expanded.as_ref() == Some(&key) {
                self.recovery.expanded = None;
                self.recovery.rows.clear();
            } else {
                self.recovery.expanded = Some(key);
                self.recovery.cursors.clear();
                self.load_recovery_page(&project_id, None);
            }
        }
        if let Some(group) = select {
            self.select_recovery_group(&project_id, &group);
        }
        if let Some(group) = retry {
            // Retrying means re-proving the mailboxes with a fresh dry
            // preflight; live migration still requires that preflight to
            // match, and goes through the normal confirmation.
            self.select_recovery_group(&project_id, &group);
            if group.state == "delta_required" {
                // A completed bulk transfer waiting for its catch-up pass:
                // run the final delta, behind the normal live confirmation.
                self.bulk_mode = crate::controller::BatchExecutionMode::Live;
                self.bulk_retry_scope = crate::controller::BulkRetryScope::DeltaRequired;
            } else {
                self.bulk_mode = crate::controller::BatchExecutionMode::Preflight;
                self.bulk_retry_scope = crate::controller::BulkRetryScope::All;
            }
            self.start_bulk();
        }
        if let Some(reason) = review {
            self.verification_attention_reason = reason;
            self.verification_offset = 0;
            self.verification_cursor = None;
            self.verification_cursor_stack.clear();
            self.active_view = WorkspaceView::Verification;
            self.refresh_ui_snapshot_now();
        }
        if let Some(view) = remediate {
            self.active_view = view;
        }
    }

    fn recovery_rows_table(&mut self, ui: &mut egui::Ui, project_id: &str) {
        let colors = self.theme_colors();
        for row in &self.recovery.rows {
            ui.label(
                RichText::new(&row.label)
                    .strong()
                    .family(crate::ui::fonts::medium()),
            );
            ui.label(
                RichText::new(format!("{} → {}", row.source_user, row.destination_user))
                    .small()
                    .color(colors.text_secondary),
            );
            let attempt = match &row.last_attempt_outcome {
                Some(outcome) => self
                    .language
                    .message("ui.recovery-last-attempt")
                    .replace("{}", outcome),
                None => self.language.message("ui.recovery-no-attempt").to_owned(),
            };
            ui.label(RichText::new(attempt).small());
            ui.label(
                RichText::new(
                    self.language
                        .message("ui.recovery-row-history")
                        .replace("{runs}", &row.runs.to_string())
                        .replace("{attempts}", &row.transfer_attempts.to_string()),
                )
                .small()
                .color(colors.text_secondary),
            );
            if let Some(detail) = &row.last_run_detail {
                ui.add(
                    egui::Label::new(RichText::new(detail).small().color(colors.text_secondary))
                        .truncate(),
                )
                .on_hover_text(detail);
            }
            if let Some(checkpoint) = &row.last_progress_checkpoint {
                ui.label(
                    RichText::new(progress_checkpoint_label(checkpoint))
                        .small()
                        .color(colors.info),
                );
            }
            ui.add_space(4.0);
        }
        let mut previous = false;
        let mut next = false;
        ui.horizontal(|ui| {
            previous = ui
                .add_enabled(
                    !self.recovery.cursors.is_empty(),
                    egui::Button::new(self.language.message("ui.recovery-previous")),
                )
                .clicked();
            next = ui
                .add_enabled(
                    self.recovery.rows.len() == RECOVERY_PAGE_ROWS as usize,
                    egui::Button::new(self.language.message("ui.recovery-next")),
                )
                .clicked();
        });
        if next {
            let after = self.recovery.rows.last().map(|row| row.rowid);
            self.recovery.cursors.push(self.recovery.page_after);
            self.load_recovery_page(project_id, after);
        } else if previous && let Some(cursor) = self.recovery.cursors.pop() {
            self.load_recovery_page(project_id, cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remediation_routes_common_blockers_to_safe_follow_up() {
        let auth = RecoveryGroup {
            state: "attention".into(),
            reason: Some(AttentionReason::AuthenticationFailed),
            count: 2,
        };
        let (consequence, label, view) = remediation(&auth);
        assert!(
            crate::ui::UiLanguage::English
                .message(consequence)
                .contains("fresh preflight")
        );
        assert_eq!(
            crate::ui::UiLanguage::English.message(label),
            "Reconnect accounts in Plan"
        );
        assert!(matches!(view, WorkspaceView::Plan));

        let quota = RecoveryGroup {
            state: "attention".into(),
            reason: Some(AttentionReason::CapacityLimited),
            count: 1,
        };
        assert!(matches!(remediation(&quota).2, WorkspaceView::Mailboxes));
    }

    #[test]
    fn unclassified_groups_are_explained_by_their_state() {
        let group = |state: &str| RecoveryGroup {
            state: state.into(),
            reason: None,
            count: 1,
        };
        let english =
            |state: &str| crate::ui::UiLanguage::English.message(remediation(&group(state)).0);
        assert!(english("delta_required").contains("final catch-up"));
        assert!(english("cancelled").contains("cancelled"));
        assert!(english("attention").contains("operator review"));
    }

    #[test]
    fn recovery_groups_the_queue_and_hands_a_group_to_mailboxes() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut app = App::from_state_path(Some(&directory.join("state.db")));
        crate::ui::debug_scene::demo_data(&mut app);
        let project_id = app.queue.project_id().unwrap().to_owned();
        app.refresh_recovery(&project_id);
        let groups = app.recovery.groups.clone();
        let failed = groups
            .iter()
            .find(|group| group.state == "failed")
            .expect("the demo queue has a failed mailbox");
        assert!(groups.iter().all(|group| group.state != "ready"));
        app.select_recovery_group(&project_id, failed);
        assert!(app.active_view == WorkspaceView::Mailboxes);
        assert_eq!(app.bulk_selection_count(), failed.count);
        assert!(app.bulk_inspector_open);

        app.recovery.expanded = Some((failed.state.clone(), failed.reason));
        app.load_recovery_page(&project_id, None);
        assert_eq!(app.recovery.rows.len(), failed.count);
        // The open group's mailbox list renders in a real frame.
        app.active_view = WorkspaceView::Recovery;
        let context = crate::ui::styled_test_context();
        let mut frame = eframe::Frame::_new_kittest();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_280.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| eframe::App::ui(&mut app, ui, &mut frame),
        );
        assert!(!output.shapes.is_empty());
        assert_eq!(
            app.recovery.expanded,
            Some((failed.state.clone(), failed.reason)),
            "rendering keeps the open group"
        );
        drop(app);
        let _ = std::fs::remove_dir_all(directory);
    }
}
