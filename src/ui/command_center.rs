//! The migration command center: one card that answers "where is every
//! mailbox, how fast is it going, and is any provider pushing back?".
//!
//! Counts come from the durable queue (`BulkQueueSummary::command_center`),
//! verified totals from durable evidence, and speed, projected finish, and
//! provider pushback from the current run's presentation telemetry. Each
//! bucket opens Mailboxes filtered to exactly the rows it counts.

use crate::App;
use crate::controller::telemetry::CooldownNote;
use crate::ui::WorkspaceView;
use crate::ui::operations::{compact_count, format_bytes, format_estimate, format_finish_clock};
use eframe::egui::{self, Color32, RichText};
use std::time::{Duration, Instant};

/// One provider (or self-hosted server) the queue talks to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderHealth {
    /// Operator-facing name: a hosted provider, or the server's host name.
    pub(crate) name: String,
    /// Longest remaining launch pause after provider pushback, if any.
    pub(crate) paused_for: Option<Duration>,
}

fn provider_display_name(provider: &str, host: &str) -> String {
    match provider {
        "gmail" => "Google Workspace".to_owned(),
        "microsoft365" => "Microsoft 365".to_owned(),
        _ => host.to_owned(),
    }
}

fn endpoint_host(endpoint: &str) -> &str {
    endpoint
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(endpoint, |(host, _)| host)
}

/// Group the queue's hosts by provider (hosted providers collapse to one row;
/// self-hosted servers stay per host) and mark each row paused while any of
/// its rate domains, or the global domain, is cooling down.
pub(crate) fn provider_health(
    hosts: &[String],
    cooldowns: &[&CooldownNote],
    now: Instant,
) -> Vec<ProviderHealth> {
    let mut rows: Vec<(String, &'static str, Vec<&str>)> = Vec::new();
    for host in hosts {
        let provider = crate::core::provider_intelligence::canonical_provider(host);
        let name = provider_display_name(provider, host);
        match rows.iter_mut().find(|(existing, ..)| *existing == name) {
            Some((_, _, members)) => members.push(host),
            None => rows.push((name, provider, vec![host])),
        }
    }
    rows.into_iter()
        .map(|(name, provider, members)| {
            let paused_for = cooldowns
                .iter()
                .filter(|note| note.until > now)
                .filter(|note| {
                    note.provider == "global"
                        || (note.provider == provider
                            && (provider != "generic"
                                || note.endpoint.as_deref().is_some_and(|endpoint| {
                                    members.iter().any(|host| {
                                        endpoint_host(endpoint).eq_ignore_ascii_case(host)
                                    })
                                })))
                })
                .map(|note| note.until.saturating_duration_since(now))
                .max();
            ProviderHealth { name, paused_for }
        })
        .collect()
}

impl App {
    /// The command center card. Shown on Overview whenever the project has a
    /// mailbox queue; it replaces hunting across Mailboxes and Activity for
    /// the operational picture.
    pub(crate) fn command_center_card(
        &mut self,
        ui: &mut egui::Ui,
        summary: crate::controller::BulkQueueSummary,
    ) {
        let colors = self.theme_colors();
        let counts = summary.command_center();
        let facts = match self.queue.fleet_facts(&self.store) {
            Ok(facts) => facts,
            Err(error) => {
                crate::ui::card(ui, |ui| {
                    ui.label(RichText::new(error).color(colors.danger));
                });
                return;
            }
        };
        let now = Instant::now();
        let running = self.running();
        let telemetry = &self.run_telemetry;
        let throughput = running.then(|| telemetry.throughput(now)).flatten();
        let eta = if running {
            let scope = self.run_scope();
            telemetry.eta(
                scope
                    .total
                    .saturating_sub(scope.finished)
                    .saturating_sub(scope.running),
                now,
            )
        } else {
            None
        };
        let health = provider_health(&facts.hosts, &telemetry.active_cooldown_notes(now), now);
        let (live_bytes, live_messages) = if running { telemetry.totals() } else { (0, 0) };
        let mut open_filter: Option<&'static str> = None;
        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            crate::ui::section_label(ui, self.language.message("ui.command-center"));
            ui.label(
                RichText::new(
                    self.language
                        .message("ui.command-center-mailboxes")
                        .replace("{}", &counts.total.to_string()),
                )
                .size(20.0)
                .strong(),
            );
            ui.add_space(6.0);
            ui.columns(2, |columns| {
                let buckets: [(&str, &str, usize, Color32, &'static str); 5] = [
                    (
                        "✓",
                        "ui.bucket-completed",
                        counts.completed,
                        colors.success,
                        "completed",
                    ),
                    (
                        "▶",
                        "ui.bucket-migrating",
                        counts.migrating,
                        colors.info,
                        "migrating",
                    ),
                    (
                        "⏸",
                        "ui.bucket-retrying",
                        counts.retrying,
                        colors.warning,
                        "retrying",
                    ),
                    (
                        "⚠",
                        "ui.bucket-needs-attention",
                        counts.needs_attention,
                        colors.danger,
                        "needs_attention",
                    ),
                    (
                        "○",
                        "ui.bucket-waiting",
                        counts.waiting,
                        colors.text_secondary,
                        "waiting",
                    ),
                ];
                // A grid keeps every count on its label's baseline.
                egui::Grid::new("command_center_buckets")
                    .num_columns(2)
                    .spacing([24.0, 8.0])
                    .show(&mut columns[0], |ui| {
                        for (glyph, key, count, color, filter) in buckets {
                            let label = self.language.message(key);
                            let text =
                                RichText::new(format!("{glyph}  {label}")).color(if count > 0 {
                                    color
                                } else {
                                    colors.text_secondary
                                });
                            let response = ui
                                .add_enabled(count > 0, egui::Button::new(text).frame(false))
                                .on_hover_text(self.language.message("ui.bucket-open-filtered"));
                            crate::ui::name_control(&response, &format!("{label}: {count}"));
                            if response.clicked() {
                                open_filter = Some(filter);
                            }
                            ui.label(RichText::new(count.to_string()).strong());
                            ui.end_row();
                        }
                    });
                let right = &mut columns[1];
                let transferred_detail = self
                    .language
                    .message("ui.command-center-transferred-detail")
                    .replace("{messages}", &compact_count(facts.messages + live_messages))
                    .replace("{verified}", &facts.evidenced.to_string());
                let finish_detail = eta
                    .map(|eta| {
                        self.language
                            .message("ui.command-center-finish-detail")
                            .replace("{}", &format_estimate(eta))
                    })
                    .unwrap_or_default();
                let figures = [
                    (
                        self.language.message("ui.command-center-transferred"),
                        format_bytes((facts.bytes + live_bytes) as f64),
                        transferred_detail,
                    ),
                    (
                        self.language.message("ui.command-center-speed"),
                        throughput.map_or_else(
                            || "—".to_owned(),
                            |rate| format!("{}/s", format_bytes(rate.bytes_per_second)),
                        ),
                        String::new(),
                    ),
                    (
                        self.language.message("ui.command-center-finish"),
                        eta.map_or_else(|| "—".to_owned(), format_finish_clock),
                        finish_detail,
                    ),
                ];
                egui::Grid::new("command_center_figures")
                    .num_columns(2)
                    .spacing([24.0, 2.0])
                    .show(right, |ui| {
                        for (label, value, detail) in figures {
                            ui.label(RichText::new(label).color(colors.text_secondary));
                            ui.label(RichText::new(value).strong());
                            ui.end_row();
                            if !detail.is_empty() {
                                ui.label("");
                                ui.label(
                                    RichText::new(detail).small().color(colors.text_secondary),
                                );
                                ui.end_row();
                            }
                        }
                    });
                if !health.is_empty() {
                    right.add_space(6.0);
                    crate::ui::section_label(right, self.language.message("ui.provider-health"));
                    egui::Grid::new("command_center_providers")
                        .num_columns(2)
                        .spacing([24.0, 6.0])
                        .show(right, |ui| {
                            for provider in &health {
                                ui.label(&provider.name);
                                match provider.paused_for {
                                    Some(remaining) => ui.label(
                                        RichText::new(
                                            self.language
                                                .message("ui.provider-throttling")
                                                .replace("{}", &format_estimate(remaining)),
                                        )
                                        .color(colors.warning),
                                    ),
                                    None if running => ui.label(
                                        RichText::new(self.language.message("ui.provider-normal"))
                                            .color(colors.success),
                                    ),
                                    None => ui.label(
                                        RichText::new(self.language.message("ui.provider-idle"))
                                            .color(colors.text_secondary),
                                    ),
                                };
                                ui.end_row();
                            }
                        });
                }
            });
        });
        match open_filter {
            // Attention rows open Recovery: why each failed, what was already
            // tried, what to do, and a retry, grouped by durable reason.
            Some("needs_attention") => self.active_view = WorkspaceView::Recovery,
            Some(filter) => self.open_mailboxes_filtered(filter),
            None => {}
        }
    }

    /// Open Mailboxes showing exactly one command-center bucket.
    pub(crate) fn open_mailboxes_filtered(&mut self, filter: &str) {
        self.bulk_search.clear();
        self.bulk_state_filter = filter.to_owned();
        self.bulk_selection_view_dirty = true;
        self.active_view = WorkspaceView::Mailboxes;
    }
}

#[cfg(test)]
mod tests {
    use super::{CooldownNote, provider_health};
    use std::time::{Duration, Instant};

    fn note(provider: &'static str, endpoint: Option<&str>, until: Instant) -> CooldownNote {
        CooldownNote {
            label: String::new(),
            until,
            provider,
            endpoint: endpoint.map(str::to_owned),
            current_limit: 1,
            configured_limit: 1,
            consecutive_failures: 1,
        }
    }

    #[test]
    fn hosted_providers_collapse_and_self_hosted_servers_stay_per_host() {
        let hosts = [
            "imap.gmail.com",
            "outlook.office365.com",
            "mail.cluster-a.example",
            "mail.cluster-b.example",
        ]
        .map(str::to_owned);
        let now = Instant::now();
        let gmail = note(
            "gmail",
            Some("imap.gmail.com:993"),
            now + Duration::from_secs(90),
        );
        let cluster_a = note(
            "generic",
            Some("mail.cluster-a.example:993"),
            now + Duration::from_secs(30),
        );
        let expired = note("microsoft365", Some("outlook.office365.com:993"), now);
        let health = provider_health(&hosts, &[&gmail, &cluster_a, &expired], now);
        let summary = health
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.paused_for.map(|pause| pause.as_secs()),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            [
                ("Google Workspace", Some(90)),
                ("Microsoft 365", None),
                ("mail.cluster-a.example", Some(30)),
                ("mail.cluster-b.example", None),
            ]
        );
    }

    #[test]
    fn a_global_pause_marks_every_provider() {
        let hosts = ["imap.gmail.com", "mail.example"].map(str::to_owned);
        let now = Instant::now();
        let global = note("global", None, now + Duration::from_secs(10));
        assert!(
            provider_health(&hosts, &[&global], now)
                .iter()
                .all(|row| row.paused_for.is_some())
        );
    }
}
