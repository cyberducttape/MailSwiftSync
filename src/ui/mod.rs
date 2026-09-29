//! Presentation primitives shared by the egui views.

use eframe::egui;

mod account;
mod activity;
mod app_state;
mod batch;
mod batch_confirmations;
mod batch_filter;
mod batch_queue;
mod batch_sheet;
#[cfg(debug_assertions)]
pub(crate) mod debug_scene;
mod engine;
pub(crate) mod fonts;
mod language;
mod output;
mod overview;
mod plan;
mod reports;
mod settings;
mod status;
mod theme;
mod verification;
mod verification_filter;
mod workspace;

#[cfg(test)]
pub(crate) use account::password_reveal_allowed;
pub(crate) use app_state::App;
pub(crate) use language::UiLanguage;
pub(crate) use output::{
    contains_ascii_case_insensitive, markdown_escape, push_visible_output, redact_secrets,
    truncate_utf8,
};
pub(crate) use status::{
    StatusMessage, StatusSeverity, customer_proof_ready, display_job_state, display_state_key,
    format_elapsed, format_phase_name, job_state_badge, needs_operator_review,
    project_health_state_counts, recommended_batch_next_action, recommended_next_action,
    status_color, successful_run_severity, successful_run_status, workflow_step_index,
};
#[cfg(test)]
pub(crate) use theme::contrast_ratio;
#[cfg(test)]
pub(crate) use theme::next_ui_scale;
pub(crate) use theme::{AppearancePreferences, ThemeColors, ThemeKind, install_style};
pub(crate) use workspace::preferred_project_id;
pub(crate) use workspace::{WorkspaceRefreshOptions, WorkspaceSnapshot, WorkspaceView};
mod app;

/// Grouped content on a raised, softly bordered surface. Every page and
/// dialog uses this instead of `Ui::group`, so the palette decides whether
/// cards are rounded and filled (modern) or square and outlined (retro).
pub(crate) fn card<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let visuals = ui.visuals();
    let radius = visuals.window_corner_radius.nw.min(8);
    egui::Frame::new()
        .fill(visuals.widgets.inactive.weak_bg_fill)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(radius)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            // Columns hand out justified layouts, which would stretch the
            // word spacing of wrapped text; cards always flow top-down.
            ui.set_min_width(ui.available_width());
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), add_contents)
                .inner
        })
}

/// The one emphasised action in a section, filled with the accent colour.
pub(crate) fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let visuals = ui.visuals();
    let fill = visuals.widgets.active.bg_fill;
    let dark = visuals.extreme_bg_color;
    let text_color =
        if theme::contrast_ratio(egui::Color32::WHITE, fill) >= theme::contrast_ratio(dark, fill) {
            egui::Color32::WHITE
        } else {
            dark
        };
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(text_color).strong())
            .fill(fill)
            .stroke(egui::Stroke::NONE),
    )
}

/// Small uppercase label that introduces a section inside a card.
pub(crate) fn section_label(ui: &mut egui::Ui, text: &str) {
    let color = ui.visuals().weak_text_color();
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .small()
            .strong()
            .color(color),
    );
}

/// Left-navigation row: full-width hit area, icon, label, and an accent bar
/// on the selected entry.
pub(crate) fn nav_item(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    label: &str,
) -> egui::Response {
    let height = 32.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let accent = visuals.widgets.active.bg_fill;
        let radius = visuals.widgets.inactive.corner_radius;
        if selected {
            ui.painter()
                .rect_filled(rect, radius, visuals.selection.bg_fill);
            let bar = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
            ui.painter().rect_filled(bar, radius, accent);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, radius, visuals.widgets.hovered.weak_bg_fill);
        }
        let text_color = if selected {
            visuals.selection.stroke.color
        } else {
            visuals.text_color()
        };
        let icon_color = if selected {
            if theme::contrast_ratio(accent, visuals.selection.bg_fill) >= 3.0 {
                accent
            } else {
                text_color
            }
        } else {
            visuals.weak_text_color()
        };
        let center_y = rect.center().y;
        ui.painter().text(
            egui::pos2(rect.left() + 14.0, center_y),
            egui::Align2::LEFT_CENTER,
            icon,
            egui::FontId::proportional(14.0),
            icon_color,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 38.0, center_y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::TextStyle::Button.resolve(ui.style()),
            text_color,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Compact rounded status tag. Drawn as one atomic widget so wrapped rows
/// can measure it and move it to the next line as a whole.
pub(crate) fn pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    let font = egui::FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let padding = egui::vec2(9.0, 3.0);
    let size = galley.size() + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = size.y / 2.0;
        ui.painter().rect(
            rect,
            radius,
            color.gamma_multiply(0.16),
            egui::Stroke::new(1.0, color.gamma_multiply(0.55)),
            egui::StrokeKind::Inside,
        );
        ui.painter().galley(rect.min + padding, galley, color);
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    response
}

/// Progress state of one step in a [`stepper`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    Done,
    Current,
    Pending,
}

/// Horizontal progress indicator: numbered markers joined by a connector,
/// with a title and optional detail under each marker. Steps share the
/// available width equally, so the row never wraps or overflows.
pub(crate) fn stepper(
    ui: &mut egui::Ui,
    steps: &[(&str, Option<&str>, StepState)],
    done: egui::Color32,
    current: egui::Color32,
) {
    if steps.is_empty() {
        return;
    }
    let pending = ui.visuals().weak_text_color();
    let line = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let on_marker = ui.visuals().extreme_bg_color;
    let color_of = |state| match state {
        StepState::Done => done,
        StepState::Current => current,
        StepState::Pending => pending,
    };
    ui.columns(steps.len(), |columns| {
        for (index, (column, (title, detail, state))) in columns.iter_mut().zip(steps).enumerate() {
            column.vertical_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 24.0),
                    egui::Sense::hover(),
                );
                let center = rect.center();
                let radius = 10.0;
                let painter = ui.painter();
                if index > 0 {
                    let previous = if *state == StepState::Pending {
                        line
                    } else {
                        done
                    };
                    painter.line_segment(
                        [
                            egui::pos2(rect.left() - 4.0, center.y),
                            egui::pos2(center.x - radius - 3.0, center.y),
                        ],
                        egui::Stroke::new(2.0, previous),
                    );
                }
                if index + 1 < steps.len() {
                    let next = if *state == StepState::Done {
                        done
                    } else {
                        line
                    };
                    painter.line_segment(
                        [
                            egui::pos2(center.x + radius + 3.0, center.y),
                            egui::pos2(rect.right() + 4.0, center.y),
                        ],
                        egui::Stroke::new(2.0, next),
                    );
                }
                let color = color_of(*state);
                let marker = if *state == StepState::Done {
                    "✔".to_owned()
                } else {
                    (index + 1).to_string()
                };
                if *state == StepState::Pending {
                    painter.circle_stroke(center, radius, egui::Stroke::new(1.5, color));
                    painter.text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        marker,
                        egui::FontId::proportional(11.0),
                        color,
                    );
                } else {
                    painter.circle_filled(center, radius, color);
                    painter.text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        marker,
                        egui::FontId::proportional(11.0),
                        on_marker,
                    );
                }
                let title_text =
                    egui::RichText::new(*title).color(if *state == StepState::Pending {
                        pending
                    } else {
                        ui.visuals().strong_text_color()
                    });
                ui.label(if *state == StepState::Current {
                    title_text.strong()
                } else {
                    title_text
                });
                if let Some(detail) = detail {
                    ui.label(egui::RichText::new(*detail).small().color(pending));
                }
            });
        }
    });
}

/// Page title with a secondary description line.
pub(crate) fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(egui::RichText::new(title).size(24.0).strong());
    ui.label(egui::RichText::new(subtitle).color(ui.visuals().weak_text_color()));
    ui.add_space(16.0);
}

/// Width of the label column in form rows, so fields line up across a card.
pub(crate) const FORM_LABEL_WIDTH: f32 = 160.0;

/// One labelled form row: a fixed-width, muted label followed by the field,
/// which may use the remaining width.
pub(crate) fn form_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    add_field: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        let color = ui.visuals().weak_text_color();
        ui.allocate_ui_with_layout(
            egui::vec2(FORM_LABEL_WIDTH, height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(FORM_LABEL_WIDTH);
                ui.add(egui::Label::new(egui::RichText::new(label).color(color)).truncate());
            },
        );
        add_field(ui)
    })
    .inner
}

/// Two-line table cell: host on top, account muted below; both truncate
/// with an ellipsis and show the full value on hover.
pub(crate) fn endpoint_cell(ui: &mut egui::Ui, host: &str, user: &str) {
    let muted = ui.visuals().weak_text_color();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.add(egui::Label::new(host).truncate())
            .on_hover_text(host);
        ui.add(egui::Label::new(egui::RichText::new(user).small().color(muted)).truncate())
            .on_hover_text(user);
    });
}
