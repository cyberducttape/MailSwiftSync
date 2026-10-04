//! Migration-plan configuration views.

use crate::App;
use crate::StatusSeverity;
use crate::controller::{
    CapabilityProbeSpec, ImapProbeEndpoint, LiveAuthProbeSpec, assess_plan,
    capability_observation_matches, spawn_capability_probe, spawn_live_auth_probe,
};
use crate::imap_probe::endpoint_for_probe;
use crate::plan_identity::fingerprint_digest as plan_fingerprint_digest;
use eframe::egui::{self, Color32, RichText};

impl App {
    pub(crate) fn live_confirmation(&mut self, ctx: &egui::Context) {
        if !self.live_confirm_open {
            return;
        }
        let mut close_requested = false;
        let response =
            egui::Modal::new(egui::Id::new("live_migration_confirmation")).show(ctx, |ui| {
                let policy = self.form.profile.destination_mutation_policy();
                let removes = policy.may_remove_destination_state();
                let modal_heading = ui.heading(
                    RichText::new(self.language.message("ui.confirm-live-migration")).color(
                        if removes {
                            self.theme_colors().danger
                        } else {
                            self.theme_colors().info
                        },
                    ),
                );
                crate::ui::name_modal(ui, &modal_heading);
                ui.label(
                    self.language
                        .text("This will invoke {} with the current credentials and rules.")
                        .replace("{}", self.language.text(self.form.engine().label())),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        self.language
                            .text("Project: {}")
                            .replace("{}", &self.form.profile.name),
                    )
                    .strong(),
                );
                ui.label(format!(
                    "{}  →  {}",
                    self.form.profile.source_host, self.form.profile.destination_host
                ));
                ui.label(
                    self.language
                        .message("ui.source-mail-not-deleted-by-default"),
                );
                if self.form.engine() == crate::core::Engine::Dovecot {
                    ui.label(
                        RichText::new(
                            self.language
                                .text("Dovecot strategy: {} — {}")
                                .replacen(
                                    "{}",
                                    self.language
                                        .text(self.form.profile.dovecot_strategy.label()),
                                    1,
                                )
                                .replacen(
                                    "{}",
                                    self.language
                                        .text(self.form.profile.dovecot_strategy.description()),
                                    1,
                                ),
                        )
                        .color(self.theme_colors().warning),
                    );
                }
                // One derived policy covers both engines: Dovecot backup
                // mirrors, imapsync --delete2 deletes, everything else keeps
                // destination-only state.
                ui.label(
                    RichText::new(self.language.text(policy.warning()))
                        .strong()
                        .color(if removes {
                            self.theme_colors().danger
                        } else {
                            self.theme_colors().text_secondary
                        }),
                );
                if removes {
                    ui.checkbox(
                        &mut self.live_destination_loss_acknowledged,
                        self.language.text(
                            "I confirm destination-only mail may be removed for this mailbox",
                        ),
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let cancel = ui.button(self.language.message("ui.cancel"));
                    if !self.live_confirm_focus_requested {
                        cancel.request_focus();
                        self.live_confirm_focus_requested = true;
                    }
                    if cancel.clicked() {
                        close_requested = true;
                    }
                    let acknowledged = !self
                        .form
                        .profile
                        .destination_mutation_policy()
                        .may_remove_destination_state()
                        || self.live_destination_loss_acknowledged;
                    let start_clicked = if removes {
                        ui.add_enabled(
                            acknowledged,
                            egui::Button::new(
                                RichText::new(
                                    self.language.message("ui.i-understand-start-migration"),
                                )
                                .color(Color32::WHITE),
                            )
                            .fill(self.theme_colors().danger),
                        )
                        .clicked()
                    } else {
                        ui.add_enabled_ui(acknowledged, |ui| {
                            crate::ui::primary_button(
                                ui,
                                self.language.message("ui.i-understand-start-migration"),
                            )
                        })
                        .inner
                        .clicked()
                    };
                    if start_clicked {
                        close_requested = true;
                        self.live_confirmed = true;
                        self.live_confirmation_plan =
                            Some(plan_fingerprint_digest(&self.form.plan_fingerprint()));
                        self.start();
                    }
                });
            });
        self.live_confirm_open = !(close_requested || response.should_close());
        if !self.live_confirm_open {
            self.live_confirm_focus_requested = false;
            self.live_destination_loss_acknowledged = false;
        }
    }
    pub(crate) fn requires_live_imaps_auth_probe(&self) -> bool {
        crate::imap_probe::fresh_imap_authentication_applies(&self.form)
    }

    pub(crate) fn invalidate_stale_capability_observation(&mut self) -> bool {
        // The fingerprint hashes the engine executable and trust files;
        // skip it when there is no probe or observation to compare.
        if self.capability_probe_fingerprint.is_none()
            && self.capability_observation_fingerprint.is_none()
        {
            return false;
        }
        let current = plan_fingerprint_digest(&self.form.plan_fingerprint());
        let in_flight_stale = self
            .capability_probe_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != current);
        let observation_stale = self.capability_observation_fingerprint.is_some()
            && !capability_observation_matches(
                self.capability_observation_fingerprint.as_deref(),
                &current,
            );
        if !(in_flight_stale || observation_stale) {
            return false;
        }
        self.capability_receiver = None;
        self.capability_probe_request_id = None;
        self.capability_probe_fingerprint = None;
        self.capability_observation_fingerprint = None;
        self.source_capabilities = None;
        self.destination_capabilities = None;
        self.preflight.clear();
        true
    }

    pub(crate) fn assess_plan(&mut self) {
        self.invalidate_stale_capability_observation();
        self.preflight = assess_plan(
            &self.form,
            self.source_capabilities.as_ref(),
            self.destination_capabilities.as_ref(),
        );
    }

    pub(crate) fn create_project(&mut self) {
        self.assess_plan();
        if self.form.profile.source_host.trim().is_empty()
            || self.form.profile.destination_host.trim().is_empty()
        {
            self.set_status(
                "Enter source and destination hosts before creating a project.",
                StatusSeverity::Warning,
            );
            return;
        }
        match self.persist_new_project(
            &self.form.profile.name,
            &self.form.profile.source_host,
            &self.form.profile.destination_host,
            &self.form.profile.source_user,
            &self.form.profile.destination_user,
        ) {
            Ok((project, job)) => {
                self.selected_project_id = Some(project.id.clone());
                self.project_id = Some(project.id);
                self.job_id = Some(job);
                self.set_status(
                    "Project created; ready for preflight review",
                    StatusSeverity::Success,
                );
            }
            Err(error) => self.set_status(
                self.language
                    .text("Could not create project: {error}")
                    .replace("{error}", &error.to_string()),
                StatusSeverity::Error,
            ),
        }
    }

    pub(crate) fn start_capability_probe(&mut self) {
        let credential_load = if self.form.dry_run {
            self.form.load_configured_keyring_credentials()
        } else {
            self.form.reload_configured_keyring_credentials()
        };
        if let Err(error) = credential_load {
            self.set_status(error, StatusSeverity::Error);
            return;
        }
        if let Err(error) = self.form.validate() {
            self.set_status(
                self.language
                    .text("Preflight input is invalid: {error}")
                    .replace("{error}", &error),
                StatusSeverity::Error,
            );
            return;
        }
        if self.form.engine() == crate::core::Engine::Dovecot {
            self.set_status(
                "Authenticated dual-endpoint IMAPS probing is for imapsync mode; Dovecot destination readiness is checked by the native dry preflight.",
                StatusSeverity::Info,
            );
            return;
        }
        if self.form.profile.source_tls == "plain" || self.form.profile.destination_tls == "plain" {
            self.set_status(
                "Authenticated capability discovery requires encrypted IMAP; plain transport remains blocked by the explicit cleartext acknowledgement gate.",
                StatusSeverity::Warning,
            );
            return;
        }
        let source = match endpoint_for_probe(
            &self.form.profile.source_host,
            &self.form.profile.source_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.set_status(
                    self.language
                        .text("Source readiness probe blocked: {error}")
                        .replace("{error}", &error),
                    StatusSeverity::Error,
                );
                return;
            }
        };
        let destination = match endpoint_for_probe(
            &self.form.profile.destination_host,
            &self.form.profile.destination_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.set_status(
                    self.language
                        .text("Destination readiness probe blocked: {error}")
                        .replace("{error}", &error),
                    StatusSeverity::Error,
                );
                return;
            }
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let plan_fingerprint = plan_fingerprint_digest(&self.form.plan_fingerprint());
        self.capability_probe_request_id = Some(request_id.clone());
        self.capability_probe_fingerprint = Some(plan_fingerprint.clone());
        self.set_status(
            "Authenticating and inspecting IMAPS readiness…",
            StatusSeverity::Info,
        );
        self.capability_receiver = Some(spawn_capability_probe(CapabilityProbeSpec {
            request_id,
            plan_fingerprint,
            source: ImapProbeEndpoint {
                endpoint: source,
                user: self.form.profile.source_user.clone(),
                password: self.form.source_password.clone(),
                auth: self.form.profile.source_auth.clone(),
                tls: self.form.profile.source_tls.clone(),
                ca_bundle: self.form.profile.source_ca_bundle.clone(),
                certificate_pin_sha256: self.form.profile.source_certificate_pin_sha256.clone(),
            },
            destination: ImapProbeEndpoint {
                endpoint: destination,
                user: self.form.profile.destination_user.clone(),
                password: self.form.destination_password.clone(),
                auth: self.form.profile.destination_auth.clone(),
                tls: self.form.profile.destination_tls.clone(),
                ca_bundle: self.form.profile.destination_ca_bundle.clone(),
                certificate_pin_sha256: self
                    .form
                    .profile
                    .destination_certificate_pin_sha256
                    .clone(),
            },
        }));
    }

    pub(crate) fn start_live_imaps_auth_probe(
        &mut self,
        plan_fingerprint: String,
        credential_fingerprint: String,
    ) {
        if self.live_auth_receiver.is_some() {
            return;
        }
        let source = match endpoint_for_probe(
            &self.form.profile.source_host,
            &self.form.profile.source_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.set_status(
                    self.language
                        .text("Live authentication probe blocked: {error}")
                        .replace("{error}", &error),
                    StatusSeverity::Error,
                );
                return;
            }
        };
        let destination = match endpoint_for_probe(
            &self.form.profile.destination_host,
            &self.form.profile.destination_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.set_status(
                    self.language
                        .text("Live authentication probe blocked: {error}")
                        .replace("{error}", &error),
                    StatusSeverity::Error,
                );
                return;
            }
        };
        self.set_status(
            "Re-authenticating encrypted IMAP endpoints before live execution…",
            StatusSeverity::Info,
        );
        self.live_auth_receiver = Some(spawn_live_auth_probe(LiveAuthProbeSpec {
            plan_fingerprint,
            credential_fingerprint,
            source: ImapProbeEndpoint {
                endpoint: source,
                user: self.form.profile.source_user.clone(),
                password: self.form.source_password.clone(),
                auth: self.form.profile.source_auth.clone(),
                tls: self.form.profile.source_tls.clone(),
                ca_bundle: self.form.profile.source_ca_bundle.clone(),
                certificate_pin_sha256: self.form.profile.source_certificate_pin_sha256.clone(),
            },
            destination: ImapProbeEndpoint {
                endpoint: destination,
                user: self.form.profile.destination_user.clone(),
                password: self.form.destination_password.clone(),
                auth: self.form.profile.destination_auth.clone(),
                tls: self.form.profile.destination_tls.clone(),
                ca_bundle: self.form.profile.destination_ca_bundle.clone(),
                certificate_pin_sha256: self
                    .form
                    .profile
                    .destination_certificate_pin_sha256
                    .clone(),
            },
        }));
    }

    pub(crate) fn preview(&mut self, ctx: &egui::Context) {
        if !self.preview {
            return;
        }
        let colors = self.theme_colors();
        egui::Window::new(self.language.message("ui.execution-plan"))
            .open(&mut self.preview)
            .default_width(670.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        self.language.message("ui.passwords-are-redacted-this-is-an-argument-list-for-review-not-a-shell-command-to-paste"),
                    )
                    .color(colors.text_secondary),
                );

                let command = self.form.preview_command();
                if let Err(validation_error) = &command {
                    ui.label(
                        RichText::new(
                            self.language
                                .text("⚠ Execution plan validation failed: {error}")
                                .replace("{error}", &validation_error.to_string()),
                        )
                            .color(colors.danger),
                    );
                    ui.label(
                        RichText::new(self.language.message("ui.fix-the-advanced-options-above-before-this-plan-can-run"))
                            .color(colors.text_secondary),
                    );
                    return;
                }

                let (exe, args) = command.expect("preview command was validated above");
                ui.label(
                    RichText::new(self.language.message("ui.executable").replace("{}", &exe))
                        .monospace(),
                );
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (index, argument) in args.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("[{index:>3}]"))
                                        .monospace()
                                        .color(colors.text_secondary),
                                );
                                ui.label(RichText::new(argument).monospace());
                            });
                        }
                    });
            });
    }

    pub(crate) fn advanced_dialog(&mut self, ctx: &egui::Context) {
        if !self.advanced_open {
            return;
        }
        let mut open = self.advanced_open;
        egui::Window::new(self.language.message("ui.advanced-migration-options"))
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(RichText::new(self.language.message("ui.these-controls-affect-the-imapsync-fallback-dovecot-native-migrations-use-d-1e32325d23")).color(self.theme_colors().text_secondary));
                ui.add_space(8.0);
                let editable = !self.running();
                ui.add_enabled_ui(editable, |ui| {
                    crate::ui::card(ui, |ui| {
                        ui.heading(self.language.message("ui.reliability-and-metadata"));
                        ui.checkbox(&mut self.form.profile.sync_internaldates, self.language.message("ui.sync-internal-dates-syncinternaldates"));
                        ui.checkbox(&mut self.form.profile.useuid, self.language.message("ui.use-message-uids-when-available-useuid"));
                        ui.checkbox(&mut self.form.profile.usecache, self.language.message("ui.use-imapsync-cache-usecache"));
                        ui.checkbox(&mut self.form.profile.allowsizemismatch, self.language.message("ui.allow-message-size-mismatch-allowsizemismatch"));
                        if self.form.engine() == crate::core::Engine::ImapSync {
                            ui.checkbox(
                                &mut self.form.profile.body_hash_verification,
                                self.language
                                    .text("Enable bounded body-content verification (forensic)"),
                            )
                            .on_hover_text(self.language.text(
                                "Downloads and hashes message bodies from both accounts. It is opt-in, bounded, and requires a stable metadata-preserving plan.",
                            ));
                        }
                        if self.form.profile.body_hash_verification {
                            ui.label(
                                RichText::new(self.language.text(
                                    "Body proof is resource-intensive. The run will stop rather than exceed either byte bound; successful evidence is labeled BodyHash.",
                                ))
                                .size(12.0)
                                .color(self.theme_colors().warning),
                            );
                            ui.horizontal(|ui| {
                                let field_label = ui.label(self.language.message("ui.maximum-body-bytes-per-message"));
                                crate::ui::LabelledField::link_label(&ui.add(
                                    egui::DragValue::new(
                                        &mut self.form.profile.body_hash_max_bytes,
                                    )
                                    .range(1..=64 * 1024 * 1024),
                                ), &field_label);
                            });
                            ui.horizontal(|ui| {
                                let field_label = ui.label(self.language.message("ui.maximum-body-bytes-per-verification"));
                                crate::ui::LabelledField::link_label(&ui.add(
                                    egui::DragValue::new(
                                        &mut self.form.profile.body_hash_max_total_bytes,
                                    )
                                    .range(1..=8 * 1024 * 1024 * 1024_u64),
                                ), &field_label);
                            });
                        }
                    });
                    ui.add_space(8.0);
                    crate::ui::card(ui, |ui| {
                        ui.heading(self.language.message("ui.performance"));
                        ui.label(RichText::new(self.language.message("ui.rate-domain-tenant-scope-explainer")).size(12.0).color(self.theme_colors().text_secondary));
                        ui.horizontal(|ui| {
                            let label = ui.label(self.language.message("ui.source-rate-tenant-scope"));
                            crate::ui::LabelledField::link_label(
                                &ui.add(egui::TextEdit::singleline(
                                    &mut self.form.profile.source_rate_tenant,
                                ).desired_width(f32::INFINITY)),
                                &label,
                            );
                        });
                        ui.horizontal(|ui| {
                            let label = ui.label(self.language.message("ui.destination-rate-tenant-scope"));
                            crate::ui::LabelledField::link_label(
                                &ui.add(egui::TextEdit::singleline(
                                    &mut self.form.profile.destination_rate_tenant,
                                ).desired_width(f32::INFINITY)),
                                &label,
                            );
                        });
                        ui.checkbox(&mut self.form.profile.fastio1, self.language.message("ui.fast-i-o-for-source-fastio1"))
                            .on_hover_text(self.language.message("ui.uses-imapsync-s-faster-source-i-o-path-test-this-with-the-provider-before-a-f268a275a4"));
                        ui.checkbox(&mut self.form.profile.fastio2, self.language.message("ui.fast-i-o-for-destination-fastio2"))
                            .on_hover_text(self.language.message("ui.uses-imapsync-s-faster-destination-i-o-path-provider-behavior-varies"));
                        ui.horizontal(|ui| {
                            let field_label = ui.label(self.language.message("ui.messages-second-target-0-unlimited"))
                                .on_hover_text(self.language.message("ui.for-a-batch-this-is-an-aggregate-target-mailswiftsync-divides-it-across-con-0765baeed4"));
                            crate::ui::LabelledField::link_label(&ui.add(egui::DragValue::new(&mut self.form.profile.max_messages_per_second).range(0..=100_000)), &field_label);
                        });
                        ui.horizontal(|ui| {
                            let field_label = ui.label(self.language.message("ui.bytes-second-target-0-unlimited"))
                                .on_hover_text(self.language.message("ui.for-a-batch-this-is-an-aggregate-target-mailswiftsync-divides-it-across-con-0765baeed4"));
                            crate::ui::LabelledField::link_label(&ui.add(egui::DragValue::new(&mut self.form.profile.max_bytes_per_second).range(0..=u64::MAX)), &field_label);
                        });
                        ui.horizontal(|ui| {
                            let field_label = ui.label(self.language.message("ui.process-timeout-hours"))
                                .on_hover_text(self.language.message("ui.maximum-wall-clock-time-for-one-engine-process-it-is-a-safety-bound-not-an-3b6f293e02"));
                            crate::ui::LabelledField::link_label(&ui.add(egui::DragValue::new(&mut self.form.profile.migration_timeout_hours).range(1..=720)), &field_label);
                        });
                        ui.label(RichText::new(self.language.message("ui.batch-targets-are-divided-across-workers-and-process-starts-are-globally-pa-f1ee89a779")).size(12.0).color(self.theme_colors().text_secondary));
                    });
                    ui.add_space(8.0);
                    if self.form.engine() == crate::core::Engine::Dovecot {
                        crate::ui::card(ui, |ui| {
                            ui.heading(self.language.message("ui.dovecot-migration-strategy"));
                            egui::ComboBox::from_id_salt("dovecot_strategy")
                .selected_text(self.language.text(self.form.profile.dovecot_strategy.label()))
                                .show_ui(ui, |ui| {
                                    for strategy in [
                                        crate::migration_plan::DovecotMigrationStrategy::InitialMirror,
                                        crate::migration_plan::DovecotMigrationStrategy::IncrementalMirror,
                                        crate::migration_plan::DovecotMigrationStrategy::FinalPreservationPass,
                                        crate::migration_plan::DovecotMigrationStrategy::DestinationAlreadyActive,
                                    ] {
                                        ui.selectable_value(
                                            &mut self.form.profile.dovecot_strategy,
                                            strategy,
                                            self.language.text(strategy.label()),
                                        );
                                    }
                                });
                            ui.label(
                                RichText::new(
                                    self.language
                                        .text(self.form.profile.dovecot_strategy.description()),
                                )
                                    .size(12.0)
                                    .color(self.theme_colors().text_secondary),
                            );
                            ui.label(
                                RichText::new(self.language.message("ui.dovecot-has-no-mailswiftsync-throttle-source-load-may-be-high-initial-backu-3c35e921d5"))
                                    .size(12.0)
                                    .color(self.theme_colors().warning),
                            );
                        });
                    } else {
                        crate::ui::card(ui, |ui| {
                            ui.heading(RichText::new(self.language.message("ui.destructive-destination-option")).color(self.theme_colors().danger));
                            ui.checkbox(&mut self.form.profile.delete2, self.language.message("ui.delete-destination-messages-missing-from-source-delete2"));
                            ui.label(RichText::new(self.language.message("ui.use-only-for-an-intentionally-exact-backup-after-a-tested-preflight-this-ca-f6b0d29b8c")).size(12.0).color(self.theme_colors().danger));
                        });
                    }
                });
                if !editable {
                    ui.label(RichText::new(self.language.message("ui.advanced-plan-settings-are-locked-while-a-migration-is-running")).color(self.theme_colors().text_secondary));
                }
                ui.add_space(8.0);
                ui.label(self.language.message("ui.the-extra-imapsync-options-field-accepts-only-the-documented-safe-tuning-an-924661fe18"));
            });
        self.advanced_open = open;
    }
}
