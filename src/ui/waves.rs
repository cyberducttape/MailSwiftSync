//! Migration waves in the GUI: the Overview waves card (status, progress,
//! schedule, approval) and the inline "create wave from selection" form on
//! Mailboxes. Storage and the live-admission approval gate live in
//! `core::waves` and `controller::batch_admission`.

use crate::App;
use crate::core::{WaveSettings, WaveStatus, WaveSummary};
use crate::ui::WorkspaceView;
use eframe::egui::{self, RichText};
use std::collections::HashSet;

#[derive(Default)]
pub(crate) struct WaveUi {
    /// The inline create form is open on Mailboxes.
    pub(crate) creating: bool,
    pub(crate) name: String,
    pub(crate) scheduled_at: String,
    pub(crate) maintenance_window: String,
    pub(crate) concurrency: String,
    /// Name recorded with an approval.
    pub(crate) approver: String,
    pub(crate) message: Option<(bool, String)>,
    /// Summaries keyed by queue generation: the query joins every member,
    /// so it must not run each frame for a 100k-mailbox project.
    cache: Option<(u64, Vec<WaveSummary>)>,
}

impl WaveUi {
    /// Read waves again on the next frame (after create/approve/delete).
    pub(crate) fn invalidate(&mut self) {
        self.cache = None;
    }
}

fn status_key(status: WaveStatus) -> &'static str {
    match status {
        WaveStatus::Draft => "ui.wave-status-draft",
        WaveStatus::Approved => "ui.wave-status-approved",
        WaveStatus::InProgress => "ui.wave-status-in-progress",
        WaveStatus::NeedsAttention => "ui.wave-status-needs-attention",
        WaveStatus::CatchUpPending => "ui.wave-status-catch-up",
        WaveStatus::Complete => "ui.wave-status-complete",
    }
}

impl App {
    fn wave_summaries(&mut self) -> Result<Vec<WaveSummary>, String> {
        let generation = self.queue.generation();
        if let Some((cached, waves)) = &self.waves.cache
            && *cached == generation
        {
            return Ok(waves.clone());
        }
        let Some(project_id) = self.queue.project_id() else {
            return Ok(Vec::new());
        };
        let waves = self
            .store
            .wave_summaries(project_id)
            .map_err(|error| format!("Could not read migration waves: {error}"))?;
        self.waves.cache = Some((generation, waves.clone()));
        Ok(waves)
    }

    /// Overview card listing the project's waves in order.
    pub(crate) fn waves_card(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        let waves = match self.wave_summaries() {
            Ok(waves) => waves,
            Err(error) => {
                crate::ui::card(ui, |ui| {
                    ui.label(RichText::new(error).color(colors.danger));
                });
                return;
            }
        };
        if self.waves.approver.is_empty() {
            self.waves.approver = std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default();
        }
        let mut select = None;
        let mut approve = None;
        let mut delete = None;
        let mut create = false;
        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                crate::ui::section_label(ui, self.language.message("ui.migration-waves"));
                create = ui
                    .small_button(self.language.message("ui.wave-create-from-selection"))
                    .clicked();
            });
            if waves.is_empty() {
                ui.label(
                    RichText::new(self.language.message("ui.waves-empty"))
                        .color(colors.text_secondary),
                );
            }
            for summary in &waves {
                let wave = &summary.wave;
                let status = summary.status();
                let status_color = match status {
                    WaveStatus::Draft => colors.text_secondary,
                    WaveStatus::Approved | WaveStatus::InProgress => colors.info,
                    WaveStatus::NeedsAttention => colors.danger,
                    WaveStatus::CatchUpPending => colors.warning,
                    WaveStatus::Complete => colors.success,
                };
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("{}. {}", wave.position + 1, wave.settings.name))
                            .strong(),
                    );
                    crate::ui::pill(ui, self.language.message(status_key(status)), status_color);
                    ui.label(
                        self.language
                            .message("ui.wave-progress")
                            .replace("{done}", &summary.completed.to_string())
                            .replace("{total}", &summary.members.to_string()),
                    );
                    if summary.needs_attention > 0 {
                        ui.label(
                            RichText::new(
                                self.language
                                    .message("ui.wave-attention")
                                    .replace("{}", &summary.needs_attention.to_string()),
                            )
                            .color(colors.danger),
                        );
                    }
                });
                let fraction = if summary.members == 0 {
                    0.0
                } else {
                    summary.completed as f32 / summary.members as f32
                };
                ui.add(egui::ProgressBar::new(fraction).desired_height(6.0));
                let mut details = Vec::new();
                if let Some(start) = &wave.settings.scheduled_at {
                    details.push(self.language.message("ui.wave-start").replace("{}", start));
                }
                if let Some(window) = &wave.settings.maintenance_window {
                    details.push(
                        self.language
                            .message("ui.wave-window")
                            .replace("{}", window),
                    );
                }
                if let Some(limit) = wave.settings.concurrency {
                    details.push(
                        self.language
                            .message("ui.wave-concurrency")
                            .replace("{}", &limit.to_string()),
                    );
                }
                details.push(
                    self.language
                        .message("ui.wave-evidence")
                        .replace("{done}", &summary.evidenced.to_string())
                        .replace("{total}", &summary.members.to_string()),
                );
                match (&wave.approved_by, &wave.approved_at) {
                    (Some(by), Some(at)) => details.push(
                        self.language
                            .message("ui.wave-approved-by")
                            .replace("{who}", by)
                            .replace("{when}", at),
                    ),
                    _ => details.push(self.language.message("ui.wave-not-approved").to_owned()),
                }
                ui.label(
                    RichText::new(details.join(" · "))
                        .small()
                        .color(colors.text_secondary),
                );
                ui.horizontal_wrapped(|ui| {
                    let running = self.running();
                    if ui
                        .add_enabled(
                            !running,
                            egui::Button::new(self.language.message("ui.wave-select")),
                        )
                        .on_hover_text(self.language.message("ui.wave-select-hint"))
                        .clicked()
                    {
                        select = Some(summary.clone());
                    }
                    if wave.approved_by.is_none() {
                        let approver = ui.add(
                            egui::TextEdit::singleline(&mut self.waves.approver)
                                .desired_width(140.0)
                                .hint_text(self.language.message("ui.wave-approver")),
                        );
                        crate::ui::name_control(
                            &approver,
                            self.language.message("ui.wave-approver"),
                        );
                        if ui
                            .add_enabled(
                                !self.waves.approver.trim().is_empty(),
                                egui::Button::new(self.language.message("ui.wave-approve")),
                            )
                            .clicked()
                        {
                            approve = Some(wave.id.clone());
                        }
                        if ui
                            .add_enabled(
                                !running,
                                egui::Button::new(self.language.message("ui.wave-delete")),
                            )
                            .clicked()
                        {
                            delete = Some(wave.id.clone());
                        }
                    }
                });
            }
            if let Some((ok, message)) = &self.waves.message {
                ui.label(RichText::new(message).color(if *ok {
                    colors.success
                } else {
                    colors.danger
                }));
            }
        });
        if create {
            self.waves.creating = true;
            self.active_view = WorkspaceView::Mailboxes;
        }
        if let Some(summary) = select {
            self.select_wave(&summary);
        }
        if let Some(wave_id) = approve {
            let result = self.store.approve_wave(&wave_id, &self.waves.approver);
            self.waves.invalidate();
            self.waves.message = Some(match result {
                Ok(()) => (true, self.language.message("ui.wave-approved").to_owned()),
                Err(error) => (false, error.to_string()),
            });
            self.refresh_ui_snapshot_now();
        }
        if let Some(wave_id) = delete {
            let result = self.store.delete_wave(&wave_id);
            self.waves.invalidate();
            self.waves.message = Some(match result {
                Ok(()) => (true, self.language.message("ui.wave-deleted").to_owned()),
                Err(error) => (false, error.to_string()),
            });
            self.bulk_wave = None;
        }
    }

    /// Select exactly a wave's members and open Mailboxes, where the usual
    /// preflight / live / catch-up actions run them as that wave.
    fn select_wave(&mut self, summary: &WaveSummary) {
        match self.store.wave_member_ids(&summary.wave.id) {
            Ok(ids) => {
                let members = ids.into_iter().collect::<HashSet<_>>();
                self.bulk_all_selected = false;
                self.bulk_selected_ids = members.clone();
                self.bulk_wave = Some((summary.wave.clone(), members));
                self.bulk_selection_view_dirty = true;
                self.bulk_state_filter = "all".into();
                self.bulk_search.clear();
                self.bulk_inspector_open = true;
                self.active_view = WorkspaceView::Mailboxes;
            }
            Err(error) => {
                self.waves.message = Some((false, format!("Could not read wave members: {error}")));
            }
        }
    }

    /// The selection's mailbox IDs, materialized for wave creation.
    fn selected_job_ids(&self) -> Result<Vec<String>, String> {
        let Some(project_id) = self.queue.project_id() else {
            return Ok(Vec::new());
        };
        if self.bulk_all_selected {
            // Select-all keeps only exclusions.
            let ids = self
                .store
                .mailbox_ids(project_id)
                .map_err(|error| format!("Could not read the selection: {error}"))?;
            Ok(ids
                .into_iter()
                .filter(|id| !self.bulk_selected_ids.contains(id))
                .collect())
        } else {
            let mut ids = self.bulk_selected_ids.iter().cloned().collect::<Vec<_>>();
            ids.sort();
            Ok(ids)
        }
    }

    /// Inline form on Mailboxes that turns the current selection into a wave.
    pub(crate) fn wave_create_form(&mut self, ui: &mut egui::Ui) {
        if !self.waves.creating {
            return;
        }
        let colors = self.theme_colors();
        let selected = self.bulk_selection_count();
        let mut create = false;
        let mut cancel = false;
        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            crate::ui::section_label(ui, self.language.message("ui.wave-new"));
            ui.label(
                RichText::new(
                    self.language
                        .message("ui.wave-new-from")
                        .replace("{}", &selected.to_string()),
                )
                .color(colors.text_secondary),
            );
            egui::Grid::new("wave_create_form")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    for (label_key, hint_key, value) in [
                        ("ui.wave-name", "ui.wave-name-hint", &mut self.waves.name),
                        (
                            "ui.wave-start-label",
                            "ui.wave-start-hint",
                            &mut self.waves.scheduled_at,
                        ),
                        (
                            "ui.wave-window-label",
                            "ui.wave-window-hint",
                            &mut self.waves.maintenance_window,
                        ),
                        (
                            "ui.wave-concurrency-label",
                            "ui.wave-concurrency-hint",
                            &mut self.waves.concurrency,
                        ),
                    ] {
                        let label = self.language.message(label_key);
                        ui.label(label);
                        let field = ui.add(
                            egui::TextEdit::singleline(value)
                                .desired_width(260.0)
                                .hint_text(self.language.message(hint_key)),
                        );
                        crate::ui::name_control(&field, label);
                        ui.end_row();
                    }
                });
            ui.horizontal(|ui| {
                create = ui
                    .add_enabled(
                        selected > 0 && !self.waves.name.trim().is_empty(),
                        egui::Button::new(self.language.message("ui.wave-create")),
                    )
                    .clicked();
                cancel = ui.button(self.language.message("ui.wave-cancel")).clicked();
            });
            if let Some((false, message)) = &self.waves.message {
                ui.label(RichText::new(message).color(colors.danger));
            }
        });
        if cancel {
            self.waves.creating = false;
            self.waves.message = None;
        }
        if create {
            let concurrency = match self.waves.concurrency.trim() {
                "" => Ok(None),
                text => text
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value >= 1)
                    .map(Some)
                    .ok_or_else(|| {
                        self.language
                            .message("ui.wave-concurrency-invalid")
                            .to_owned()
                    }),
            };
            let result = concurrency.and_then(|concurrency| {
                let project_id = self
                    .queue
                    .project_id()
                    .map(str::to_owned)
                    .ok_or_else(|| "Import a mailbox list first.".to_owned())?;
                let ids = self.selected_job_ids()?;
                self.store
                    .create_wave(
                        &project_id,
                        &WaveSettings {
                            name: self.waves.name.clone(),
                            scheduled_at: Some(self.waves.scheduled_at.clone()),
                            maintenance_window: Some(self.waves.maintenance_window.clone()),
                            concurrency,
                        },
                        &ids,
                    )
                    .map_err(|error| error.to_string())
            });
            match result {
                Ok(wave) => {
                    self.waves = WaveUi {
                        approver: std::mem::take(&mut self.waves.approver),
                        message: Some((
                            true,
                            self.language
                                .message("ui.wave-created")
                                .replace("{}", &wave.settings.name),
                        )),
                        ..WaveUi::default()
                    };
                    self.active_view = WorkspaceView::Overview;
                }
                Err(error) => self.waves.message = Some((false, error)),
            }
        }
    }
}
