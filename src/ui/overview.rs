//! Shared Overview workflow presentation.

use crate::App;
use crate::core;
use crate::ui::format_phase_name;
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn lifecycle_stepper(&self, ui: &mut egui::Ui) {
        let phases = [
            core::Phase::Discovery,
            core::Phase::Preflight,
            core::Phase::Pilot,
            core::Phase::Seed,
            core::Phase::CatchUp,
            core::Phase::FinalDelta,
            core::Phase::Verification,
            core::Phase::Complete,
        ];
        let current = match self.active_project_id() {
            None => core::Phase::Discovery,
            Some(_) => self
                .ui_snapshot
                .project
                .as_ref()
                .map(|project| project.phase)
                .unwrap_or(core::Phase::Discovery),
        };
        let current_index = phases
            .iter()
            .position(|phase| *phase == current)
            .unwrap_or(usize::MAX);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("MIGRATION LIFECYCLE").size(11.0).strong().color(self.theme_colors().text_secondary));
                if current == core::Phase::Attention {
                    ui.label(RichText::new("ATTENTION REQUIRED").strong().color(self.theme_colors().danger));
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (index, phase) in phases.iter().enumerate() {
                    if index > 0 {
                        ui.label(RichText::new("→").color(self.theme_colors().text_secondary));
                    }
                    let color = if current_index != usize::MAX && index < current_index {
                        self.theme_colors().success
                    } else if index == current_index {
                        self.theme_colors().info
                    } else {
                        self.theme_colors().text_secondary
                    };
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            if current_index != usize::MAX && index < current_index { "✓" }
                            else if index == current_index { "●" } else { "○" },
                            format_phase_name(*phase)
                        ))
                        .strong()
                        .color(color),
                    );
                }
            });
            if current == core::Phase::Attention {
                ui.label(RichText::new("A mailbox or run needs operator review. Normal lifecycle progress is paused until it is resolved.").size(11.0).color(self.theme_colors().danger));
            }
        });
    }
}
