use crate::{auth_method_is_oauth, credentials::SecretString};
use eframe::egui::{self, Color32, RichText};

use super::ThemeColors;

pub(crate) fn password_reveal_allowed(editable: bool, requested: bool) -> bool {
    editable && requested
}

pub(crate) fn password_visibility_id(title: &str) -> egui::Id {
    egui::Id::new(("password_visibility", title))
}

/// Render one source or destination account editor. The form controller owns
/// the values; this module owns only their presentation and validation hints.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_account(
    ui: &mut egui::Ui,
    title: &str,
    host: &mut String,
    user: &mut String,
    auth_method: &mut String,
    password: &mut SecretString,
    password_required: bool,
    saved_credential: bool,
    color: Color32,
) {
    let editable = ui.ctx().data(|data| {
        data.get_temp::<bool>(egui::Id::new("plan_controls_enabled"))
            .unwrap_or(true)
    });
    let danger = if ui.visuals().dark_mode {
        ThemeColors::dark().danger
    } else {
        ThemeColors::light().danger
    };
    let inline_error = |ui: &mut egui::Ui, label: &str, value: &str, required: bool| {
        let message = if required && value.trim().is_empty() {
            Some(format!("{label} is required."))
        } else if !value.is_empty() && value.chars().any(char::is_control) {
            Some(format!("{label} contains an invalid control character."))
        } else {
            None
        };
        if let Some(message) = message {
            ui.label(RichText::new(message).color(danger).size(11.0));
        }
    };
    ui.group(|ui| {
        ui.heading(RichText::new(title).color(color));
        ui.label(
            RichText::new("IMAP connection").size(11.0).color(if ui.visuals().dark_mode {
                ThemeColors::dark().text_secondary
            } else {
                ThemeColors::light().text_secondary
            }),
        );
        ui.horizontal(|ui| {
            ui.label("Server");
            ui.add_enabled(editable, egui::TextEdit::singleline(host));
        });
        inline_error(ui, "Server", host, true);
        ui.horizontal(|ui| {
            ui.label("User");
            ui.add_enabled(editable, egui::TextEdit::singleline(user));
        });
        inline_error(ui, "User", user, true);
        ui.horizontal(|ui| {
            ui.label("Authentication");
            egui::ComboBox::from_id_salt(("auth_method", title))
                .selected_text(if auth_method_is_oauth(auth_method) {
                    "OAuth 2.0 / XOAUTH2"
                } else {
                    "Password"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(auth_method, "password".into(), "Password");
                    ui.selectable_value(auth_method, "oauth2".into(), "OAuth 2.0 / XOAUTH2");
                });
        });
        if auth_method_is_oauth(auth_method) {
            ui.label(
                RichText::new(
                    "Use a currently valid provider-issued access token with IMAP scope. Tokens are session-only unless stored in the OS keyring; MailSwiftSync does not request consent or refresh tokens yet.",
                )
                .size(11.0)
                .color(if ui.visuals().dark_mode {
                    ThemeColors::dark().text_secondary
                } else {
                    ThemeColors::light().text_secondary
                }),
            );
        }
        ui.horizontal(|ui| {
            ui.label(if auth_method_is_oauth(auth_method) {
                "Access token"
            } else {
                "Password"
            });
            let visibility_id = password_visibility_id(title);
            let visible = ui.ctx().data_mut(|data| {
                let requested = data.get_temp::<bool>(visibility_id).unwrap_or(false);
                if !editable {
                    data.remove::<bool>(visibility_id);
                }
                password_reveal_allowed(editable, requested)
            });
            ui.add_enabled(
                editable,
                egui::TextEdit::singleline(password.as_mut_string()).password(!visible),
            );
            if ui
                .add_enabled(
                    editable,
                    egui::Button::new(if visible { "Hide" } else { "Show" }),
                )
                .clicked()
            {
                ui.ctx()
                    .data_mut(|data| data.insert_temp(visibility_id, !visible));
            }
        });
        if saved_credential && password.is_empty() {
            ui.label(
                RichText::new(if auth_method_is_oauth(auth_method) {
                    "Saved OAuth credential configured; session token not required."
                } else {
                    "Saved credential configured; session password not required."
                })
                .color(if ui.visuals().dark_mode {
                    ThemeColors::dark().success
                } else {
                    ThemeColors::light().success
                })
                .size(12.0),
            );
        } else {
            inline_error(
                ui,
                if auth_method_is_oauth(auth_method) {
                    "Access token"
                } else {
                    "Password"
                },
                password.as_str(),
                password_required && !saved_credential,
            );
        }
    });
}
