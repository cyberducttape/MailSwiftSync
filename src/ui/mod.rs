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
pub(crate) mod engine;
pub(crate) mod fonts;
mod keyring_ops;
mod language;
mod operations;
mod output;
mod overview;
mod plan;
mod qualification;
mod reports;
mod settings;
mod snapshot_worker;
mod status;
mod theme;
mod verification;
mod verification_filter;
mod workspace;

#[cfg(test)]
pub(crate) use account::password_reveal_allowed;
pub(crate) use app_state::{App, OAuthAuthorizationMessage};
pub(crate) use batch_queue::mailbox_import_available;
pub(crate) use language::UiLanguage;
pub(crate) use output::{
    contains_case_insensitive, fold_search_text, markdown_escape, push_visible_output,
    redact_secrets, truncate_utf8,
};
#[cfg(test)]
pub(crate) use status::recommended_next_action;
pub(crate) use status::{
    StatusMessage, StatusSeverity, customer_proof_ready, display_job_state, display_state_key,
    format_elapsed, format_phase_name, job_state_badge, needs_operator_review,
    project_health_state_counts, recommended_workspace_action, status_color,
    successful_run_severity, successful_run_status,
};
#[cfg(test)]
pub(crate) use theme::contrast_ratio;
#[cfg(test)]
pub(crate) use theme::next_ui_scale;
pub(crate) use theme::{AppearancePreferences, ThemeColors, ThemeKind, install_style};
pub(crate) use workspace::preferred_project_id;
pub(crate) use workspace::{OwnedRefreshOptions, WorkspaceSnapshot, WorkspaceView};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceIcon {
    Overview,
    Plan,
    Mailboxes,
    Activity,
    Verification,
    Projects,
    Settings,
}

fn paint_workspace_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: WorkspaceIcon,
    color: egui::Color32,
    scale: f32,
) {
    use egui::{Pos2, Stroke, Vec2};
    let s = scale;
    let stroke = Stroke::new(1.55 * (s / 16.0), color);
    let line = |points: &[Pos2]| {
        painter.add(egui::Shape::line(points.to_vec(), stroke));
    };
    let p = |x: f32, y: f32| center + Vec2::new(x * s / 16.0, y * s / 16.0);
    match icon {
        WorkspaceIcon::Overview => {
            for (x, y) in [(-6.0, -6.0), (1.0, -6.0), (-6.0, 1.0), (1.0, 1.0)] {
                let a = p(x, y);
                let b = p(x + 5.0, y + 5.0);
                painter.line_segment([a, Pos2::new(b.x, a.y)], stroke);
                painter.line_segment([Pos2::new(b.x, a.y), b], stroke);
                painter.line_segment([b, Pos2::new(a.x, b.y)], stroke);
                painter.line_segment([Pos2::new(a.x, b.y), a], stroke);
            }
        }
        WorkspaceIcon::Plan => {
            line(&[
                p(-5.0, -7.0),
                p(2.0, -7.0),
                p(6.0, -3.0),
                p(6.0, 7.0),
                p(-5.0, 7.0),
                p(-5.0, -7.0),
            ]);
            line(&[p(2.0, -7.0), p(2.0, -3.0), p(6.0, -3.0)]);
            line(&[p(-2.0, 0.0), p(3.0, 0.0)]);
            line(&[p(-2.0, 3.0), p(3.0, 3.0)]);
        }
        WorkspaceIcon::Mailboxes => {
            line(&[
                p(-7.0, -5.0),
                p(7.0, -5.0),
                p(7.0, 5.0),
                p(-7.0, 5.0),
                p(-7.0, -5.0),
            ]);
            line(&[p(-7.0, -4.0), p(0.0, 1.0), p(7.0, -4.0)]);
        }
        WorkspaceIcon::Activity => {
            line(&[
                p(-7.0, 0.0),
                p(-4.0, 0.0),
                p(-2.0, -5.0),
                p(1.0, 5.0),
                p(3.0, -2.0),
                p(4.0, 0.0),
                p(7.0, 0.0),
            ]);
        }
        WorkspaceIcon::Verification => {
            line(&[
                p(0.0, -7.0),
                p(6.0, -5.0),
                p(5.0, 2.0),
                p(0.0, 7.0),
                p(-5.0, 2.0),
                p(-6.0, -5.0),
                p(0.0, -7.0),
            ]);
            line(&[p(-3.0, 0.0), p(-1.0, 2.0), p(3.0, -2.0)]);
        }
        WorkspaceIcon::Projects => {
            painter.circle_stroke(p(0.0, -5.0), 6.0 * s / 16.0, stroke);
            painter.circle_stroke(p(0.0, 0.0), 6.0 * s / 16.0, stroke);
            painter.circle_stroke(p(0.0, 5.0), 6.0 * s / 16.0, stroke);
            line(&[p(-6.0, -5.0), p(-6.0, 5.0)]);
            line(&[p(6.0, -5.0), p(6.0, 5.0)]);
        }
        WorkspaceIcon::Settings => {
            painter.circle_stroke(center, 5.5 * s / 16.0, stroke);
            painter.circle_stroke(center, 2.0 * s / 16.0, stroke);
            for (x, y) in [
                (0.0, -7.0),
                (0.0, 7.0),
                (-7.0, 0.0),
                (7.0, 0.0),
                (-5.0, -5.0),
                (5.0, -5.0),
                (-5.0, 5.0),
                (5.0, 5.0),
            ] {
                painter.line_segment(
                    [
                        center + Vec2::new(x * s / 16.0, y * s / 16.0),
                        center + Vec2::new(x * s / 16.0 * 0.78, y * s / 16.0 * 0.78),
                    ],
                    stroke,
                );
            }
        }
    }
}

pub(crate) fn nav_item(
    ui: &mut egui::Ui,
    selected: bool,
    icon: WorkspaceIcon,
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
        paint_workspace_icon(
            ui.painter(),
            egui::pos2(rect.left() + 20.0, center_y),
            icon,
            icon_color,
            16.0,
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

/// Compact navigation row for narrow windows. The tooltip retains the full
/// accessible name while the icon stays centered in the reduced rail.
pub(crate) fn nav_icon_item(
    ui: &mut egui::Ui,
    selected: bool,
    icon: WorkspaceIcon,
    label: &str,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        if selected {
            ui.painter().rect_filled(
                rect,
                visuals.widgets.inactive.corner_radius,
                visuals.selection.bg_fill,
            );
            ui.painter().rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
                visuals.widgets.inactive.corner_radius,
                visuals.widgets.active.bg_fill,
            );
        } else if response.hovered() {
            ui.painter().rect_filled(
                rect,
                visuals.widgets.inactive.corner_radius,
                visuals.widgets.hovered.weak_bg_fill,
            );
        }
        let color = if selected {
            visuals.selection.stroke.color
        } else {
            visuals.weak_text_color()
        };
        paint_workspace_icon(ui.painter(), rect.center(), icon, color, 17.0);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    response
        .on_hover_text(label)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
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
    // A step title must never break mid-word. When the columns are too
    // narrow (large UI scale, long translations), list the steps vertically.
    let column_width = ui.available_width() / steps.len() as f32;
    let widest_title = steps
        .iter()
        .map(|(title, _, _)| {
            egui::WidgetText::from(egui::RichText::new(*title).strong())
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Body,
                )
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    if widest_title + 8.0 > column_width {
        for (index, (title, detail, state)) in steps.iter().enumerate() {
            let color = color_of(*state);
            let marker = match state {
                StepState::Done => "✔".to_owned(),
                StepState::Current => "▶".to_owned(),
                StepState::Pending => (index + 1).to_string(),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(marker).strong().color(color));
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
                    ui.label(
                        egui::RichText::new(format!("· {detail}"))
                            .small()
                            .color(pending),
                    );
                }
            });
        }
        return;
    }
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
/// A field that can take its accessible name from a visible label.
pub(crate) trait LabelledField {
    fn link_label(&self, label: &egui::Response);
}

impl LabelledField for egui::Response {
    fn link_label(&self, label: &egui::Response) {
        let _ = self.clone().labelled_by(label.id);
    }
}

impl<T> LabelledField for egui::InnerResponse<T> {
    fn link_label(&self, label: &egui::Response) {
        self.response.link_label(label);
    }
}

/// Expose a modal's content as a named, modal dialog. egui renders modals
/// as plain containers, so without this a screen reader is never told that
/// a confirmation opened or what it is about.
pub(crate) fn name_modal(ui: &egui::Ui, heading: &egui::Response) {
    let heading_id = heading.id.accesskit_id();
    ui.ctx().accesskit_node_builder(ui.unique_id(), |builder| {
        builder.set_role(egui::accesskit::Role::Dialog);
        builder.set_modal();
        builder.push_labelled_by(heading_id);
    });
}

/// Give a control an accessible name when no visible label sits beside it.
pub(crate) fn name_control(response: &egui::Response, name: &str) {
    response
        .ctx
        .accesskit_node_builder(response.id, |builder| builder.set_label(name.to_owned()));
}

/// Label column + field. The label is linked to the field so screen readers
/// announce it as the field's name; `add_field` must return the field's
/// response (there is deliberately no `()` implementation).
pub(crate) fn form_row<R: LabelledField>(
    ui: &mut egui::Ui,
    label: &str,
    add_field: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        let color = ui.visuals().weak_text_color();
        let label = ui
            .allocate_ui_with_layout(
                egui::vec2(FORM_LABEL_WIDTH, height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(FORM_LABEL_WIDTH);
                    ui.add(egui::Label::new(egui::RichText::new(label).color(color)).truncate())
                },
            )
            .inner;
        let field = add_field(ui);
        field.link_label(&label);
        field
    })
    .inner
}

/// Give `response` keyboard focus the first frame a dialog is shown, so a
/// confirmation opens on its safe choice. Pair with `reset_initial_focus`
/// when the dialog closes.
pub(crate) fn focus_on_open(ui: &egui::Ui, response: &egui::Response, dialog: egui::Id) {
    let key = dialog.with("initial_focus_done");
    if !ui
        .ctx()
        .data(|data| data.get_temp::<bool>(key).unwrap_or(false))
    {
        response.request_focus();
        ui.ctx().data_mut(|data| data.insert_temp(key, true));
    }
}

pub(crate) fn reset_initial_focus(ctx: &egui::Context, dialog: egui::Id) {
    ctx.data_mut(|data| data.remove::<bool>(dialog.with("initial_focus_done")));
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

#[cfg(test)]
mod accessibility_audit;

#[cfg(test)]
mod dialog_reachability_tests {
    /// Every dialog flag must be set somewhere in production UI code. A
    /// refactor once removed the only buttons that opened the credential
    /// and engine dialogs, leaving OAuth refresh configuration unreachable.
    #[test]
    fn every_dialog_has_a_production_opener() {
        let production = [
            include_str!("app.rs"),
            include_str!("overview.rs"),
            include_str!("plan.rs"),
            include_str!("batch.rs"),
            include_str!("activity.rs"),
            include_str!("verification.rs"),
            include_str!("settings.rs"),
            include_str!("workspace.rs"),
            include_str!("account.rs"),
            include_str!("engine.rs"),
        ]
        .concat();
        for flag in [
            "keyring_open = true",
            "engine_open = true",
            "settings_open = true",
            "projects_open = true",
            "advanced_open = true",
            "preview = true",
            "bulk_clear_confirm_open = true",
        ] {
            assert!(production.contains(flag), "no production UI sets `{flag}`");
        }
    }
}
