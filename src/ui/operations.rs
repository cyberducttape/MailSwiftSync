//! Activity's live operations view: throughput, progress, ETA, workers,
//! retries, cooldowns, and the latest actionable failure for the current run.
//!
//! All figures come from `RunTelemetry`, which is presentation-only and
//! content-free. Every forward-looking number is labelled as an estimate.

use crate::App;
use crate::controller::telemetry::{SPARKLINE_BUCKET, WindowRisk, window_risk};
use crate::ui::WorkspaceView;
use eframe::egui::{self, RichText};
use std::time::{Duration, Instant};

/// Active mailbox rows shown before collapsing the rest into a count.
const MAX_ACTIVE_ROWS: usize = 8;

/// Decimal byte units, matching how providers state quotas and sizes.
pub(crate) fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Coarse, honest duration: estimates do not deserve second precision once
/// they exceed a few minutes.
pub(crate) fn format_estimate(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 90 {
        format!("{seconds} s")
    } else if seconds < 60 * 60 {
        format!("{} min", seconds.div_ceil(60))
    } else {
        let minutes = seconds.div_ceil(60);
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    }
}

pub(crate) fn compact_count(value: u64) -> String {
    match value {
        0..1_000 => value.to_string(),
        1_000..1_000_000 => format!("{:.1}K", value as f64 / 1_000.0),
        _ => format!("{:.1}M", value as f64 / 1_000_000.0),
    }
}

/// Finish time on the local wall clock for an estimate.
pub(crate) fn format_finish_clock(eta: Duration) -> String {
    chrono::TimeDelta::from_std(eta)
        .ok()
        .and_then(|delta| chrono::Local::now().checked_add_signed(delta))
        .map(|at| at.format("%H:%M").to_string())
        .unwrap_or_default()
}

pub(crate) struct RunScope {
    pub(crate) total: usize,
    pub(crate) finished: usize,
    pub(crate) running: usize,
}

impl App {
    fn job_label(&self, job_id: &str) -> String {
        self.queue
            .project_id()
            .and_then(|project_id| self.store.queue_row(project_id, job_id).ok().flatten())
            .map(|job| job.label)
            .unwrap_or_else(|| {
                let user = self.form.profile.source_user.trim();
                if user.is_empty() {
                    self.language.text("This mailbox").to_owned()
                } else {
                    user.to_owned()
                }
            })
    }

    pub(crate) fn run_scope(&self) -> RunScope {
        let telemetry = &self.run_telemetry;
        // Queue state covers mailboxes still authenticating; telemetry covers
        // engines already reporting. Either alone can lag the other.
        let queue_running = if !self.running() {
            0
        } else if let Some(run) = self
            .active_run
            .as_ref()
            .filter(|run| !run.batch_job_ids.is_empty())
        {
            run.batch_job_ids
                .iter()
                .filter(|id| self.queue.transient_state(id) == Some("running"))
                .count()
        } else if telemetry.scope() > 1 {
            self.queue.transient_count("running")
        } else {
            1
        };
        let reporting = if self.running() {
            telemetry.active_jobs().len()
        } else {
            0
        };
        let running = queue_running.max(reporting).min(telemetry.scope());
        let finished = if telemetry.scope() <= 1 && !self.running() {
            telemetry.scope()
        } else {
            telemetry.completed().min(telemetry.scope())
        };
        RunScope {
            total: telemetry.scope(),
            finished,
            running,
        }
    }

    /// The live operations card. Hidden until the first run of the session.
    pub(crate) fn operations_panel(&mut self, ui: &mut egui::Ui) {
        if self.run_telemetry.started_at().is_none() {
            return;
        }
        let now = Instant::now();
        let colors = self.theme_colors();
        let scope = self.run_scope();
        let waiting = scope
            .total
            .saturating_sub(scope.finished)
            .saturating_sub(scope.running);
        let telemetry = &self.run_telemetry;
        let throughput = telemetry.throughput(now);
        let eta = if self.running() {
            telemetry.eta(waiting, now)
        } else {
            None
        };
        let (bytes, messages) = telemetry.totals();
        let series = telemetry.throughput_series(now);
        let pending_retries = telemetry.pending_retries(now).len();
        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.heading(self.language.text("Migration operations"));
                if self.form.engine() == crate::core::Engine::Dovecot {
                    ui.label(
                        RichText::new(self.language.text(
                            "Dovecot runs do not report transfer progress; counts below stay empty.",
                        ))
                        .small()
                        .color(colors.text_secondary),
                    );
                }
            });
            ui.add_space(6.0);
            let limit = if scope.total > 1 {
                crate::migration_plan::effective_batch_concurrency(
                    self.form.profile.batch_concurrency,
                )
            } else {
                1
            };
            tile_rows(ui, 5, 150.0, |index, ui| match index {
                0 => {
                    stat_tile(
                        ui,
                        self.language.text("Estimated time remaining"),
                        &eta.map_or_else(
                            || {
                                if self.running() {
                                    self.language.text("Estimating…").to_owned()
                                } else {
                                    "—".to_owned()
                                }
                            },
                            |eta| format!("~{}", format_estimate(eta)),
                        ),
                        &eta.map(|eta| {
                            self.language
                                .text("Finishes around {}")
                                .replace("{}", &format_finish_clock(eta))
                        })
                        .unwrap_or_else(|| {
                            self.language
                                .text("Needs a few minutes of transfer data")
                                .to_owned()
                        }),
                        26.0,
                    );
                }
                1 => {
                    stat_tile(
                        ui,
                        self.language.text("Throughput · 5-min average"),
                        &throughput.map_or_else(
                            || "—".to_owned(),
                            |rate| format!("{}/s", format_bytes(rate.bytes_per_second)),
                        ),
                        &throughput.map_or_else(
                            || self.language.text("Needs 10 s of transfer data").to_owned(),
                            |rate| {
                                self.language.text("{} msgs/min").replace(
                                "{}",
                                &compact_count((rate.messages_per_second * 60.0).round() as u64),
                            )
                            },
                        ),
                        20.0,
                    );
                    sparkline(ui, &series, colors.info, self.language);
                }
                2 => {
                    stat_tile(
                        ui,
                        self.language.text("Transferred"),
                        &format_bytes(bytes as f64),
                        &self
                            .language
                            .text("{} messages")
                            .replace("{}", &compact_count(messages)),
                        20.0,
                    );
                }
                3 => {
                    stat_tile(
                        ui,
                        self.language.text("Mailboxes remaining"),
                        &format!(
                            "{} / {}",
                            scope.total.saturating_sub(scope.finished),
                            scope.total
                        ),
                        &self
                            .language
                            .text("{} finished · {} waiting")
                            .replacen("{}", &scope.finished.to_string(), 1)
                            .replacen("{}", &waiting.to_string(), 1),
                        20.0,
                    );
                }
                4 => {
                    stat_tile(
                        ui,
                        self.language.text("Active workers"),
                        &format!("{} / {}", scope.running, limit),
                        &self
                            .language
                            .text("Retries pending: {}")
                            .replace("{}", &pending_retries.to_string()),
                        20.0,
                    );
                }
                _ => {}
            });
            ui.add_space(8.0);
            self.maintenance_window_row(ui, eta);
            self.active_transfers(ui, now);
            self.retries_and_cooldowns(ui, now);
        });
        self.latest_failure_card(ui);
        ui.add_space(8.0);
    }

    fn maintenance_window_row(&mut self, ui: &mut egui::Ui, eta: Option<Duration>) {
        let colors = self.theme_colors();
        ui.horizontal_wrapped(|ui| {
            let field_label = ui.label(self.language.text("Maintenance window"));
            crate::ui::LabelledField::link_label(
                &ui.add(
                    egui::TextEdit::singleline(&mut self.activity_window_spec)
                        .hint_text("22:00-06:00")
                        .desired_width(150.0),
                ),
                &field_label,
            );
            let spec = self.activity_window_spec.trim();
            if spec.is_empty() {
                ui.label(
                    RichText::new(
                        self.language
                            .text("Enter the cutover window to check whether the estimate fits."),
                    )
                    .small()
                    .color(colors.text_secondary),
                );
                return;
            }
            let (icon, text, color) =
                match crate::maintenance_window::MaintenanceWindow::parse(spec) {
                    Err(_) => (
                        "!",
                        self.language
                            .text("Use HH:MM-HH:MM, optionally @Mon,Tue")
                            .to_owned(),
                        colors.warning,
                    ),
                    Ok(window) => match window.closes_in_now() {
                        None => (
                            "○",
                            self.language.text("Outside the window").to_owned(),
                            colors.text_secondary,
                        ),
                        Some(None) => (
                            "✓",
                            self.language.text("Window is open all day").to_owned(),
                            colors.success,
                        ),
                        Some(Some(closes_in)) => {
                            let closes = format_estimate(closes_in);
                            match eta.map(|eta| window_risk(eta, closes_in)) {
                                None => (
                                    "○",
                                    self.language
                                        .text("Window closes in {}; no estimate yet")
                                        .replace("{}", &closes),
                                    colors.text_secondary,
                                ),
                                Some(WindowRisk::OnTrack) => (
                                    "✓",
                                    self.language
                                        .text("On schedule · window closes in {}")
                                        .replace("{}", &closes),
                                    colors.success,
                                ),
                                Some(WindowRisk::Tight) => (
                                    "!",
                                    self.language
                                        .text("Tight · window closes in {}")
                                        .replace("{}", &closes),
                                    colors.warning,
                                ),
                                Some(WindowRisk::Overrun) => (
                                    "✕",
                                    self.language
                                        .text("Likely to overrun · window closes in {}")
                                        .replace("{}", &closes),
                                    colors.danger,
                                ),
                            }
                        }
                    },
                };
            // Status always pairs a glyph and words with its color.
            ui.label(
                RichText::new(format!("{icon} {text}"))
                    .strong()
                    .color(color),
            );
        });
    }

    fn active_transfers(&self, ui: &mut egui::Ui, now: Instant) {
        let telemetry = &self.run_telemetry;
        let active = telemetry.active_jobs();
        if active.is_empty() {
            return;
        }
        let colors = self.theme_colors();
        ui.add_space(8.0);
        crate::ui::section_label(ui, self.language.text("Active transfers"));
        egui::Grid::new("activity_active_transfers")
            .num_columns(4)
            .spacing([14.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for (job_id, job) in active.iter().take(MAX_ACTIVE_ROWS) {
                    let progress = &job.progress;
                    ui.label(self.job_label(job_id));
                    let fraction = progress.estimated_total_bytes().and_then(|total| {
                        (total > 0).then(|| progress.bytes_copied as f64 / total as f64)
                    });
                    meter(ui, fraction, colors.info);
                    ui.label(match progress.estimated_total_bytes() {
                        Some(total) => self
                            .language
                            .text("{} of ~{}")
                            .replacen("{}", &format_bytes(progress.bytes_copied as f64), 1)
                            .replacen("{}", &format_bytes(total as f64), 1),
                        None => format_bytes(progress.bytes_copied as f64),
                    });
                    let status = match telemetry
                        .pending_retries(now)
                        .into_iter()
                        .find(|(id, _)| id == job_id)
                    {
                        Some((_, retry)) => RichText::new(
                            self.language
                                .text("Retry {} in {} · {}")
                                .replacen("{}", &retry.attempt.to_string(), 1)
                                .replacen(
                                    "{}",
                                    &format_estimate(retry.retry_at.saturating_duration_since(now)),
                                    1,
                                )
                                .replacen("{}", retry.failure_class, 1),
                        )
                        .color(colors.warning),
                        None => RichText::new(match progress.messages_left {
                            Some(left) => self
                                .language
                                .text("{} messages left")
                                .replace("{}", &compact_count(left)),
                            None => self
                                .language
                                .text("{} messages copied")
                                .replace("{}", &compact_count(progress.messages_copied)),
                        })
                        .color(colors.text_secondary),
                    };
                    ui.label(status);
                    ui.end_row();
                }
            });
        if active.len() > MAX_ACTIVE_ROWS {
            ui.label(
                RichText::new(
                    self.language
                        .text("and {} more in flight")
                        .replace("{}", &(active.len() - MAX_ACTIVE_ROWS).to_string()),
                )
                .small()
                .color(colors.text_secondary),
            );
        }
    }

    fn retries_and_cooldowns(&self, ui: &mut egui::Ui, now: Instant) {
        let telemetry = &self.run_telemetry;
        let colors = self.theme_colors();
        // Retries for mailboxes that have not reported progress yet; those
        // with progress already show their countdown in the transfer table.
        let waiting_retries = telemetry
            .pending_retries(now)
            .into_iter()
            .filter(|(id, _)| telemetry.job(id).is_none_or(|job| job.finished))
            .collect::<Vec<_>>();
        let cooldowns = telemetry.active_cooldowns(now);
        if waiting_retries.is_empty() && cooldowns.is_empty() {
            return;
        }
        ui.add_space(8.0);
        crate::ui::section_label(ui, self.language.text("Retries and provider cooldowns"));
        for (job_id, retry) in waiting_retries {
            ui.label(
                RichText::new(
                    self.language
                        .text("! {} · retry {} in {} · {}")
                        .replacen("{}", &self.job_label(job_id), 1)
                        .replacen("{}", &retry.attempt.to_string(), 1)
                        .replacen(
                            "{}",
                            &format_estimate(retry.retry_at.saturating_duration_since(now)),
                            1,
                        )
                        .replacen("{}", retry.failure_class, 1),
                )
                .color(colors.warning),
            );
        }
        for (endpoint, remaining) in cooldowns {
            ui.label(
                RichText::new(
                    self.language
                        .text("⏸ {} · new launches paused for {} after provider throttling")
                        .replacen("{}", endpoint, 1)
                        .replacen("{}", &format_estimate(remaining), 1),
                )
                .color(colors.warning),
            );
        }
    }

    fn latest_failure_card(&mut self, ui: &mut egui::Ui) {
        let Some(failure) = self.run_telemetry.latest_failure().cloned() else {
            return;
        };
        let colors = self.theme_colors();
        ui.add_space(8.0);
        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            crate::ui::section_label(ui, self.language.text("Latest actionable failure"));
            let who = failure
                .job_id
                .as_deref()
                .map_or_else(|| self.job_label(""), |id| self.job_label(id));
            ui.label(
                RichText::new(format!(
                    "✕ {who} · {} · {}",
                    self.language
                        .text(crate::ui::display_job_state(&failure.state)),
                    self.language
                        .text("{} ago")
                        .replace("{}", &format_estimate(failure.at.elapsed()))
                ))
                .strong()
                .color(colors.danger),
            );
            if !failure.detail.is_empty() {
                ui.label(RichText::new(&failure.detail).color(colors.text_secondary));
            }
            let (label, view) = if failure.job_id.is_some() {
                (
                    self.language.text("Review mailbox  →"),
                    WorkspaceView::Mailboxes,
                )
            } else {
                (
                    self.language.text("Open migration plan  →"),
                    WorkspaceView::Plan,
                )
            };
            if ui.button(label).clicked() {
                if let Some(job_id) = failure.job_id.as_deref()
                    && let Some(job) = self.queue.row_by_id(&self.store, job_id)
                {
                    self.bulk_search = job.label;
                }
                self.active_view = view;
            }
        });
    }
}

/// Fixed-width, left-aligned tiles that wrap onto further rows when the
/// window is narrow or the UI scale is large, instead of squeezing (and
/// justifying) text into five columns.
fn tile_rows(
    ui: &mut egui::Ui,
    count: usize,
    min_width: f32,
    mut cell: impl FnMut(usize, &mut egui::Ui),
) {
    let gap = ui.spacing().item_spacing.x * 2.0;
    let available = ui.available_width();
    let per_row = (((available + gap) / (min_width + gap)).floor() as usize).clamp(1, count.max(1));
    let width = (available - gap * (per_row - 1) as f32) / per_row as f32;
    for start in (0..count).step_by(per_row) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for index in start..(start + per_row).min(count) {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        cell(index, ui);
                    },
                );
            }
        });
        ui.add_space(6.0);
    }
}

/// Label · value · supporting line, in text tokens (never series color).
fn stat_tile(ui: &mut egui::Ui, label: &str, value: &str, detail: &str, size: f32) {
    let muted = ui.visuals().weak_text_color();
    ui.label(RichText::new(label).small().color(muted));
    ui.label(RichText::new(value).size(size).strong());
    ui.label(RichText::new(detail).small().color(muted));
}

/// Fill = progress in the accent; track = a lighter step of the same hue.
/// Unknown totals render an empty track rather than a guessed fraction.
fn meter(ui: &mut egui::Ui, fraction: Option<f64>, accent: egui::Color32) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(160.0, 8.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, accent.gamma_multiply(0.25));
    if let Some(fraction) = fraction {
        let mut fill = rect;
        fill.set_width((rect.width() * fraction.clamp(0.0, 1.0) as f32).max(4.0));
        painter.rect_filled(fill, 4.0, accent);
        response.on_hover_text(format!("{:.0}%", fraction.clamp(0.0, 1.0) * 100.0));
    }
}

/// Single-series throughput trend: one 2px line, no legend (the tile names
/// it), and a hover readout for the bucket under the pointer.
fn sparkline(
    ui: &mut egui::Ui,
    series: &[f64],
    color: egui::Color32,
    language: crate::ui::UiLanguage,
) {
    let width = ui.available_width().min(220.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::hover());
    let painter = ui.painter();
    let baseline = ui.visuals().widgets.noninteractive.bg_stroke.color;
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, baseline),
    );
    if series.len() < 2 {
        return;
    }
    let max = series.iter().copied().fold(0.0_f64, f64::max).max(1.0);
    let step = rect.width() / (series.len() - 1) as f32;
    let point = |index: usize, value: f64| {
        egui::pos2(
            rect.left() + step * index as f32,
            rect.bottom() - (value / max) as f32 * (rect.height() - 2.0),
        )
    };
    let points = series
        .iter()
        .enumerate()
        .map(|(index, value)| point(index, *value))
        .collect::<Vec<_>>();
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, color)));
    if let Some(pointer) = response.hover_pos() {
        let index =
            (((pointer.x - rect.left()) / step).round().max(0.0) as usize).min(series.len() - 1);
        let marker = point(index, series[index]);
        painter.circle_filled(marker, 4.0, color);
        let ago = SPARKLINE_BUCKET.as_secs() * (series.len() - 1 - index) as u64;
        response.on_hover_text(
            language
                .text("{}/s · {} ago")
                .replacen("{}", &format_bytes(series[index]), 1)
                .replacen("{}", &format_estimate(Duration::from_secs(ago)), 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_count, format_bytes, format_estimate};
    use std::time::Duration;

    #[test]
    fn byte_and_duration_formats_stay_compact() {
        assert_eq!(format_bytes(999.0), "999 B");
        assert_eq!(format_bytes(2_400_000.0), "2.4 MB");
        assert_eq!(format_bytes(345_000_000_000.0), "345 GB");
        assert_eq!(format_estimate(Duration::from_secs(45)), "45 s");
        assert_eq!(format_estimate(Duration::from_secs(301)), "6 min");
        assert_eq!(format_estimate(Duration::from_secs(5_000)), "1 h 24 min");
        assert_eq!(compact_count(12_900), "12.9K");
    }
}
