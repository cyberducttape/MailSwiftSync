//! Operator-facing provider qualification and pre-migration simulation.

use crate::{App, core, provider::ProviderPreset};
use eframe::egui::{self, RichText};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QualificationRoute {
    GmailToMicrosoft365,
    Microsoft365ToGmail,
    FastmailToMicrosoft365,
    Other,
}

fn qualification_route(source: ProviderPreset, destination: ProviderPreset) -> QualificationRoute {
    match (source, destination) {
        (ProviderPreset::GoogleWorkspace, ProviderPreset::Microsoft365) => {
            QualificationRoute::GmailToMicrosoft365
        }
        (ProviderPreset::Microsoft365, ProviderPreset::GoogleWorkspace) => {
            QualificationRoute::Microsoft365ToGmail
        }
        (ProviderPreset::Fastmail, ProviderPreset::Microsoft365) => {
            QualificationRoute::FastmailToMicrosoft365
        }
        _ => QualificationRoute::Other,
    }
}

impl App {
    pub(crate) fn provider_qualification_card(&self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        let source = self.language.text(self.source_provider.label());
        let destination = self.language.text(self.destination_provider.label());
        let route = qualification_route(self.source_provider, self.destination_provider);
        let route_label = match route {
            QualificationRoute::GmailToMicrosoft365 => "Google Workspace → Microsoft 365",
            QualificationRoute::Microsoft365ToGmail => "Microsoft 365 → Google Workspace",
            QualificationRoute::FastmailToMicrosoft365 => "Fastmail → Microsoft 365",
            QualificationRoute::Other => "{} → {}",
        };

        crate::ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                crate::ui::section_label(ui, self.language.text("PROVIDER QUALIFICATION"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(self.language.text("UNQUALIFIED"))
                            .strong()
                            .color(colors.warning),
                    );
                });
            });
            let pair = if route == QualificationRoute::Other {
                route_label
                    .replacen("{}", source, 1)
                    .replacen("{}", destination, 1)
            } else {
                route_label.to_owned()
            };
            ui.label(RichText::new(pair).size(16.0).strong());
            ui.label(
                RichText::new(self.language.text("COMMUNITY / UNQUALIFIED PATH"))
                    .strong()
                    .color(colors.warning),
            );
            ui.label(
                RichText::new(self.language.text(
                    "The generic IMAP migration path is available, but this provider pair has not passed the MailSwiftSync production qualification suite.",
                ))
                .color(colors.text_secondary),
            );
            ui.label(
                RichText::new(self.language.text(
                    "Last qualification: none · no live provider evidence is currently bundled.",
                ))
                .small()
                .color(colors.text_secondary),
            );
            ui.add_space(5.0);
            ui.label(
                RichText::new(self.language.text(
                    "Qualification requires tested authentication, folder inventory and mapping, independent verification, interruption recovery, throttling recovery, and large-mailbox coverage.",
                ))
                .small()
                .color(colors.text_secondary),
            );
        });
    }

    pub(crate) fn migration_simulation_card(&self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        let policy = self.form.profile.destination_mutation_policy();
        let source_authenticated = self.source_capabilities.is_some();
        let destination_authenticated = self.destination_capabilities.is_some();
        let source_folders = self
            .source_capabilities
            .as_ref()
            .filter(|caps| caps.inventory_complete)
            .map(|caps| caps.mailbox_count);
        let destination_folders = self
            .destination_capabilities
            .as_ref()
            .filter(|caps| caps.inventory_complete)
            .map(|caps| caps.mailbox_count);
        let namespace_assessment = self
            .source_capabilities
            .as_ref()
            .and_then(|source| source.namespace.as_ref())
            .zip(
                self.destination_capabilities
                    .as_ref()
                    .and_then(|destination| destination.namespace.as_ref()),
            )
            .map(|(source, destination)| {
                if source.personal != destination.personal {
                    "namespace prefix/delimiter warning"
                } else if !source.shared.is_empty()
                    || !destination.shared.is_empty()
                    || !source.other_users.is_empty()
                    || !destination.other_users.is_empty()
                {
                    "shared or other-user namespaces detected"
                } else {
                    "personal namespaces match"
                }
            })
            .unwrap_or("namespace mapping not yet assessed");
        let risks = simulation_risks(
            namespace_assessment != "personal namespaces match"
                && namespace_assessment != "namespace mapping not yet assessed",
            self.destination_capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.quota_exceeded),
            source_authenticated && destination_authenticated,
            source_folders.is_some() && destination_folders.is_some(),
        )
        .into_iter()
        .map(|risk| self.language.text(risk))
        .collect::<Vec<_>>()
        .join(" · ");
        let source_auth = if source_authenticated {
            self.language
                .text("Authenticated in the latest readiness check")
        } else {
            self.language.text("Not yet verified")
        };
        let destination_auth = if destination_authenticated {
            self.language
                .text("Authenticated in the latest readiness check")
        } else {
            self.language.text("Not yet verified")
        };
        let source_auth_method = auth_method_label(&self.form.profile.source_auth);
        let destination_auth_method = auth_method_label(&self.form.profile.destination_auth);
        let destination_behavior = if self.form.dry_run {
            self.language
                .text("No destination writes are intended during preflight")
        } else {
            self.language.text(policy.warning())
        };
        let engine = match self.form.engine() {
            core::Engine::Dovecot => "Dovecot doveadm",
            _ => "imapsync",
        };
        let engine_path = match self.form.engine() {
            core::Engine::Dovecot => self.form.profile.doveadm_path.as_str(),
            _ => self.form.profile.imapsync_path.as_str(),
        };
        let engine_path = if engine_path.trim().is_empty() {
            self.language.text("resolved from PATH").to_owned()
        } else {
            engine_path.to_owned()
        };
        let scale = if let (Some(source), Some(destination)) = (source_folders, destination_folders)
        {
            self.language
                .text("Folder inventory: {} source · {} destination")
                .replacen("{}", &source.to_string(), 1)
                .replacen("{}", &destination.to_string(), 1)
        } else {
            self.language
                .text("Message count and data volume are not known yet")
                .to_owned()
        };
        let scope = if self.bulk_jobs.is_empty() {
            self.language.text("One mailbox plan template").to_owned()
        } else {
            self.language
                .text("{} selected of {} queued mailbox(es)")
                .replacen("{}", &self.bulk_selected_ids.len().to_string(), 1)
                .replacen("{}", &self.bulk_jobs.len().to_string(), 1)
        };

        crate::ui::card(ui, |ui| {
            crate::ui::section_label(ui, self.language.text("PRE-MIGRATION SIMULATION"));
            ui.label(
                RichText::new(self.language.text(
                    "Plan preview only — estimates are shown only when supported by observed data.",
                ))
                .small()
                .color(colors.text_secondary),
            );
            egui::Grid::new("migration_simulation_facts")
                .num_columns(2)
                .spacing([18.0, 7.0])
                .show(ui, |ui| {
                    simulation_row(ui, self.language.text("SCOPE"), &scope);
                    simulation_row(
                        ui,
                        self.language.text("SOURCE"),
                        self.language
                            .text("Read-only; source messages are not deleted by default"),
                    );
                    simulation_row(ui, self.language.text("DESTINATION"), destination_behavior);
                    simulation_row(
                        ui,
                        self.language.text("ENGINE"),
                        &format!(
                            "{engine} · {engine_path} · {}",
                            self.language.text("engine version not yet checked")
                        ),
                    );
                    simulation_row(
                        ui,
                        self.language.text("MAPPING"),
                        &format!(
                            "{} · {}",
                            if self.form.profile.automap {
                                self.language.text("standard-folder automapping enabled")
                            } else {
                                self.language.text("standard-folder automapping disabled")
                            },
                            self.language.text(namespace_assessment)
                        ),
                    );
                    simulation_row(
                        ui,
                        self.language.text("AUTH"),
                        &format!(
                            "Source ({}): {} · Destination ({}): {}",
                            self.language.text(source_auth_method),
                            source_auth,
                            self.language.text(destination_auth_method),
                            destination_auth
                        ),
                    );
                    simulation_row(ui, self.language.text("ESTIMATED SCALE"), &scale);
                    simulation_row(ui, self.language.text("RISKS"), &risks);
                });
            ui.add_space(5.0);
            ui.label(
                RichText::new(self.language.text("PROPOSED EXECUTION"))
                    .small()
                    .strong(),
            );
            ui.label(
                RichText::new(self.language.text(
                    "Preflight → small pilot → seed → catch-up → final delta → independent verification",
                ))
                .color(colors.text_secondary),
            );
        });
    }
}

fn simulation_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).small().strong());
    ui.label(value);
    ui.end_row();
}

fn auth_method_label(method: &str) -> &'static str {
    match method {
        "oauth2" => "OAuth 2.0",
        "password" => "Password / app password",
        _ => "Configured authentication",
    }
}

fn simulation_risks(
    namespace_warning: bool,
    destination_quota_exhausted: bool,
    authentication_verified: bool,
    inventory_available: bool,
) -> Vec<&'static str> {
    let mut risks = Vec::new();
    if destination_quota_exhausted {
        risks.push("High: destination quota is currently exhausted");
    }
    if namespace_warning {
        risks.push("Medium: namespace or shared-folder behavior needs review");
    }
    if !authentication_verified {
        risks.push("Readiness pending: authenticate both endpoints");
    }
    if !inventory_available {
        risks.push("Inventory pending: mailbox scale is unknown");
    }
    if risks.is_empty() {
        risks.push("No risk is established yet; provider-specific review is still required");
    }
    risks
}

#[cfg(test)]
mod tests {
    use super::{QualificationRoute, qualification_route, simulation_risks};
    use crate::provider::ProviderPreset;

    #[test]
    fn only_the_declared_release_routes_are_named_qualification_pairs() {
        assert_eq!(
            qualification_route(
                ProviderPreset::GoogleWorkspace,
                ProviderPreset::Microsoft365
            ),
            QualificationRoute::GmailToMicrosoft365
        );
        assert_eq!(
            qualification_route(
                ProviderPreset::Microsoft365,
                ProviderPreset::GoogleWorkspace
            ),
            QualificationRoute::Microsoft365ToGmail
        );
        assert_eq!(
            qualification_route(ProviderPreset::Fastmail, ProviderPreset::Microsoft365),
            QualificationRoute::FastmailToMicrosoft365
        );
        assert_eq!(
            qualification_route(ProviderPreset::GoogleWorkspace, ProviderPreset::GenericImap),
            QualificationRoute::Other
        );
    }

    #[test]
    fn simulation_reports_observed_risks_and_does_not_claim_unknown_safety() {
        assert_eq!(
            simulation_risks(false, false, false, false),
            [
                "Readiness pending: authenticate both endpoints",
                "Inventory pending: mailbox scale is unknown"
            ]
        );
        assert_eq!(
            simulation_risks(true, true, true, true),
            [
                "High: destination quota is currently exhausted",
                "Medium: namespace or shared-folder behavior needs review"
            ]
        );
        assert_eq!(
            simulation_risks(false, false, true, true),
            ["No risk is established yet; provider-specific review is still required"]
        );
    }
}
