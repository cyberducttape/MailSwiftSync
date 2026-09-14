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
        let mut open = self.live_confirm_open;
        let mut close_requested = false;
        egui::Window::new("Confirm live migration")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(
                    RichText::new("Destination changes require confirmation")
                        .color(self.theme_colors().danger),
                );
                ui.label(format!(
                    "This will invoke {} with the current credentials and rules.",
                    self.form.engine().label()
                ));
                ui.add_space(8.0);
                ui.label(RichText::new(format!("Project: {}", self.form.profile.name)).strong());
                ui.label(format!(
                    "{}  →  {}",
                    self.form.profile.source_host, self.form.profile.destination_host
                ));
                let deletion_enabled = self.form.profile.delete2;
                ui.label("Source mail: not deleted by default");
                ui.label(
                    RichText::new(format!(
                        "Destination deletion: {}",
                        if deletion_enabled {
                            "ENABLED ⚠"
                        } else {
                            "disabled"
                        }
                    ))
                    .color(if deletion_enabled {
                        self.theme_colors().danger
                    } else {
                        self.theme_colors().text_secondary
                    }),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_requested = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("I understand — start migration")
                                    .color(Color32::WHITE),
                            )
                            .fill(self.theme_colors().danger),
                        )
                        .clicked()
                    {
                        close_requested = true;
                        self.live_confirmed = true;
                        self.live_confirmation_plan =
                            Some(plan_fingerprint_digest(&self.form.plan_fingerprint()));
                        self.start();
                    }
                });
            });
        self.live_confirm_open = open && !close_requested;
    }
    pub(crate) fn requires_live_imaps_auth_probe(&self) -> bool {
        crate::imap_probe::fresh_imap_authentication_applies(&self.form)
    }

    pub(crate) fn invalidate_stale_capability_observation(&mut self) -> bool {
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
        match self.store.create_project_with_mailbox(
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
                format!("Could not create project: {error}"),
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
                format!("Preflight input is invalid: {error}"),
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
                    format!("Source readiness probe blocked: {error}"),
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
                    format!("Destination readiness probe blocked: {error}"),
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
                    format!("Live authentication probe blocked: {error}"),
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
                    format!("Live authentication probe blocked: {error}"),
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
        egui::Window::new("Execution plan")
            .open(&mut self.preview)
            .default_width(670.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Passwords are redacted. This is an argument list for review, not a shell command to paste.",
                    )
                    .color(colors.text_secondary),
                );
                let (exe, args) = self.form.command(true);
                ui.label(RichText::new(format!("Executable: {exe}")).monospace());
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
        egui::Window::new("Advanced migration options")
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules.").color(self.theme_colors().text_secondary));
                ui.add_space(8.0);
                let editable = !self.running();
                ui.add_enabled_ui(editable, |ui| {
                    ui.group(|ui| {
                        ui.heading("Reliability and metadata");
                        ui.checkbox(&mut self.form.profile.sync_internaldates, "Sync internal dates  (--syncinternaldates)");
                        ui.checkbox(&mut self.form.profile.useuid, "Use message UIDs when available  (--useuid)");
                        ui.checkbox(&mut self.form.profile.usecache, "Use imapsync cache  (--usecache)");
                        ui.checkbox(&mut self.form.profile.allowsizemismatch, "Allow message-size mismatch  (--allowsizemismatch)");
                    });
                    ui.add_space(8.0);
                    ui.group(|ui| {
                        ui.heading("Performance");
                        ui.checkbox(&mut self.form.profile.fastio1, "Fast I/O for source  (--fastio1)")
                            .on_hover_text("Uses imapsync's faster source I/O path; test this with the provider before a production cutover.");
                        ui.checkbox(&mut self.form.profile.fastio2, "Fast I/O for destination  (--fastio2)")
                            .on_hover_text("Uses imapsync's faster destination I/O path; provider behavior varies.");
                        ui.horizontal(|ui| {
                            ui.label("Messages/second target (0 = unlimited)")
                                .on_hover_text("For a batch this is an aggregate target: MailSwiftSync divides it across concurrent imapsync workers. A single run uses the value unchanged.");
                            ui.add(egui::DragValue::new(&mut self.form.profile.max_messages_per_second).range(0..=100_000));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Bytes/second target (0 = unlimited)")
                                .on_hover_text("For a batch this is an aggregate target: MailSwiftSync divides it across concurrent imapsync workers. A single run uses the value unchanged.");
                            ui.add(egui::DragValue::new(&mut self.form.profile.max_bytes_per_second).range(0..=u64::MAX));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Process timeout (hours)")
                                .on_hover_text("Maximum wall-clock time for one engine process. It is a safety bound, not an estimate of completion time.");
                            ui.add(egui::DragValue::new(&mut self.form.profile.migration_timeout_hours).range(1..=720));
                        });
                        ui.label(RichText::new("Batch targets are divided across workers and process starts are globally paced; provider-side limits still take precedence. A finite target must be at least the worker count.").size(11.0).color(self.theme_colors().text_secondary));
                    });
                    ui.add_space(8.0);
                    ui.group(|ui| {
                        ui.heading(RichText::new("Destructive destination option").color(self.theme_colors().danger));
                        ui.checkbox(&mut self.form.profile.delete2, "Delete destination messages missing from source  (--delete2)");
                        ui.label(RichText::new("Use only for an intentionally exact backup after a tested preflight. This can remove destination mail.").size(11.0).color(self.theme_colors().danger));
                    });
                });
                if !editable {
                    ui.label(RichText::new("Advanced plan settings are locked while a migration is running.").color(self.theme_colors().text_secondary));
                }
                ui.add_space(8.0);
                ui.label("The Extra imapsync options field accepts only the documented safe tuning and diagnostic allowlist. Connection, credential, TLS, destructive, logging, and unknown flags are rejected.");
            });
        self.advanced_open = open;
    }
}
