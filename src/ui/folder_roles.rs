//! Plan-page section that proposes explicit special-folder mappings from the
//! roles both servers advertise, so a plan can avoid uncertifiable automap.

use crate::App;
use crate::migration_plan::folder_roles::{
    FolderRoleNote, SpecialFolderProposal, propose_special_folder_mappings,
};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn special_folder_mapping_section(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        if self.form.profile.automap {
            ui.label(
                RichText::new(self.language.message("ui.automap-not-certifiable"))
                    .color(colors.warning),
            );
        }
        let running = self.folder_role_receiver.is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !running && !self.running(),
                    egui::Button::new(self.language.message("ui.suggest-special-folder-mappings")),
                )
                .clicked()
            {
                self.start_special_folder_probe();
            }
            if running {
                ui.spinner();
                ui.label(self.language.message("ui.special-folder-probe-running"));
            }
        });
        if let Some(error) = &self.folder_role_error {
            ui.label(RichText::new(error).color(colors.danger));
        }
        let Some(proposal) = self.folder_role_proposal.clone() else {
            return;
        };
        if proposal.rules.is_empty() {
            ui.label(self.language.message("ui.special-folder-no-proposals"));
        }
        for rule in &proposal.rules {
            ui.label(format!("{} → {}", rule.source, rule.destination));
        }
        for note in &proposal.notes {
            ui.label(RichText::new(self.folder_role_note_text(note)).color(colors.warning));
        }
        if !proposal.rules.is_empty() {
            let label = if self.form.profile.automap {
                self.language
                    .message("ui.apply-special-folder-mappings-and-disable-automap")
            } else {
                self.language.message("ui.apply-special-folder-mappings")
            }
            .replace("{}", &proposal.rules.len().to_string());
            if crate::ui::primary_button(ui, &label).clicked() {
                self.form
                    .profile
                    .folder_mapping_rules
                    .extend(proposal.rules.iter().cloned());
                // Explicit mappings are what the verifier can replay.
                self.form.profile.automap = false;
                self.folder_role_proposal = None;
            }
        }
    }

    fn folder_role_note_text(&self, note: &FolderRoleNote) -> String {
        let role = |role: crate::migration_plan::folder_roles::FolderRole| {
            self.language.message(role.label_key())
        };
        match note {
            FolderRoleNote::Ambiguous {
                role: kind,
                source_side: true,
            } => self
                .language
                .message("ui.folder-role-note-ambiguous-source")
                .replace("{role}", role(*kind)),
            FolderRoleNote::Ambiguous {
                role: kind,
                source_side: false,
            } => self
                .language
                .message("ui.folder-role-note-ambiguous-destination")
                .replace("{role}", role(*kind)),
            FolderRoleNote::MissingOnDestination { role: kind, source } => self
                .language
                .message("ui.folder-role-note-missing")
                .replace("{role}", role(*kind))
                .replace("{folder}", source),
            FolderRoleNote::AllMail { source } => self
                .language
                .message("ui.folder-role-note-all-mail")
                .replace("{folder}", source),
        }
    }

    /// Read both accounts' folder roles off the UI thread. Credentials come
    /// from the plan (session values, or configured keyring/OAuth references).
    fn start_special_folder_probe(&mut self) {
        let mut form = self.form.clone();
        let existing = self.form.profile.folder_mapping_rules.clone();
        let source_label = self.language.message("ui.source");
        let destination_label = self.language.message("ui.destination");
        let failure = self.language.message("ui.special-folder-probe-failed");
        let (sender, receiver) = std::sync::mpsc::channel();
        self.folder_role_error = None;
        self.folder_role_proposal = None;
        let spawned = std::thread::Builder::new()
            .name("mailswiftsync-folder-roles".into())
            .spawn(move || {
                let result = (|| {
                    if form.source_password.is_empty() || form.destination_password.is_empty() {
                        form.reload_configured_keyring_credentials()?;
                    }
                    let (source, destination) = crate::imap_probe::list_both_account_folders(&form)
                        .map_err(|error| {
                            let side = match error.side {
                                crate::imap_probe::MailSide::Source => source_label,
                                crate::imap_probe::MailSide::Destination => destination_label,
                            };
                            failure
                                .replace("{side}", side)
                                .replace("{error}", &error.message)
                        })?;
                    Ok(propose_special_folder_mappings(
                        &source,
                        &destination,
                        &existing,
                    ))
                })();
                let _ = sender.send(result);
            });
        match spawned {
            Ok(_) => self.folder_role_receiver = Some(receiver),
            Err(error) => self.folder_role_error = Some(error.to_string()),
        }
    }

    pub(crate) fn complete_special_folder_probe(
        &mut self,
        result: Result<SpecialFolderProposal, String>,
    ) {
        match result {
            Ok(proposal) => self.folder_role_proposal = Some(proposal),
            Err(error) => self.folder_role_error = Some(error),
        }
    }
}
