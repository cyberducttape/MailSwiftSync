//! Drill-down from a mailbox's evidence to its folder-level differences and
//! the individual missing, additional, changed, misplaced, or duplicated
//! messages, with filtering, keyset pagination, and CSV export.
//!
//! The ledger never stores folder names or Message-IDs. Differences are
//! grouped by project-scoped folder digest; a digest is shown as its folder
//! name only when a verification in this process observed that name.

use std::collections::HashMap;

use eframe::egui::{self, RichText};

use crate::core::{
    MailboxEvidence, MessageMismatch, MismatchFilter, MismatchFolderSummary, MismatchType,
    ReportMailboxSnapshot, StoredMismatch,
};

const PAGE_SIZE: usize = 50;
/// Bound one CSV export; the durable detail is itself capped by the
/// verifier's 64 MiB mismatch budget.
const MAX_EXPORT_ROWS: usize = 250_000;
/// Bound the process-local name map; folder inventories are bounded too.
const MAX_OBSERVED_FOLDER_NAMES: usize = 100_000;

const TYPES: [MismatchType; 5] = [
    MismatchType::Missing,
    MismatchType::Extra,
    MismatchType::MessageIdOnly,
    MismatchType::PresentWrongFolder,
    MismatchType::Duplicated,
];

fn type_key(mismatch_type: MismatchType) -> &'static str {
    match mismatch_type {
        MismatchType::Missing => "ui.mismatch-missing",
        MismatchType::Extra => "ui.mismatch-extra",
        MismatchType::MessageIdOnly => "ui.mismatch-changed",
        MismatchType::PresentWrongFolder => "ui.mismatch-wrong-folder",
        MismatchType::Duplicated => "ui.mismatch-duplicated",
    }
}

/// Folder names seen in this process, keyed by `core::folder_digest`.
#[derive(Default)]
pub(crate) struct ObservedFolderNames(HashMap<String, String>);

impl ObservedFolderNames {
    /// Remember the folder names of mismatches about to be persisted.
    pub(crate) fn observe(&mut self, project_id: &str, mismatches: &[MessageMismatch]) {
        for folder in mismatches
            .iter()
            .flat_map(|mismatch| [&mismatch.source_folder, &mismatch.destination_folder])
            .flatten()
        {
            if self.0.len() >= MAX_OBSERVED_FOLDER_NAMES {
                return;
            }
            self.0
                .entry(crate::core::folder_digest(project_id, folder))
                .or_insert_with(|| folder.clone());
        }
    }

    pub(crate) fn name(&self, digest: &str) -> Option<&str> {
        self.0.get(digest).map(String::as_str)
    }

    /// A folder digest as the operator sees it: its name when known,
    /// otherwise a short digest label.
    fn label(&self, digest: Option<&str>, unknown: &str) -> String {
        match digest {
            None | Some("") => unknown.to_owned(),
            Some(digest) => self.name(digest).map_or_else(
                // `get` keeps a malformed non-ASCII ledger value from
                // panicking on a char boundary.
                || format!("#{}", digest.get(..12).unwrap_or(digest)),
                str::to_owned,
            ),
        }
    }
}

/// Folder rows of the summary: `(digest, counts in TYPES order)`.
pub(crate) fn folder_rows(summary: &[MismatchFolderSummary]) -> Vec<(String, [u64; 5])> {
    let mut rows: Vec<(String, [u64; 5])> = Vec::new();
    for entry in summary {
        let Some(index) = TYPES.iter().position(|known| *known == entry.mismatch_type) else {
            // Durable state may outlive the UI's known mismatch vocabulary.
            // Preserve the rest of the report instead of crashing the UI when
            // a newer or malformed row is encountered.
            continue;
        };
        match rows.last_mut() {
            Some((digest, counts)) if *digest == entry.folder_digest => {
                counts[index] += entry.count;
            }
            _ => {
                let mut counts = [0; 5];
                counts[index] = entry.count;
                rows.push((entry.folder_digest.clone(), counts));
            }
        }
    }
    rows
}

#[derive(Default)]
pub(crate) struct MismatchInspector {
    /// `(job, evidence run)` the loaded data belongs to.
    key: Option<(String, String)>,
    filter: MismatchFilter,
    after: i64,
    previous: Vec<i64>,
    rows: Vec<StoredMismatch>,
    more: bool,
    summary: Vec<MismatchFolderSummary>,
    error: Option<String>,
    stale: bool,
}

impl MismatchInspector {
    fn select(&mut self, job_id: &str, run_id: &str) {
        let key = (job_id.to_owned(), run_id.to_owned());
        if self.key.as_ref() != Some(&key) {
            *self = Self {
                key: Some(key),
                stale: true,
                ..Self::default()
            };
        }
    }

    fn set_filter(&mut self, filter: MismatchFilter) {
        if self.filter != filter {
            self.filter = filter;
            self.after = 0;
            self.previous.clear();
            self.stale = true;
        }
    }

    fn load(&mut self, store: &crate::core::StateStore) {
        let Some((job_id, run_id)) = self.key.clone() else {
            return;
        };
        self.stale = false;
        let result = store
            .mismatch_folder_summary(&job_id, &run_id)
            .and_then(|summary| {
                let (rows, more) = store.message_mismatch_page(
                    &job_id,
                    &run_id,
                    &self.filter,
                    self.after,
                    PAGE_SIZE,
                )?;
                Ok((summary, rows, more))
            });
        match result {
            Ok((summary, rows, more)) => {
                self.summary = summary;
                self.rows = rows;
                self.more = more;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

impl crate::App {
    /// The per-mailbox migration evidence card: transfer, metadata, flags,
    /// and body-hash facts with an operator conclusion.
    pub(crate) fn mailbox_evidence_card_ui(
        &mut self,
        ui: &mut egui::Ui,
        mailbox: &ReportMailboxSnapshot,
    ) {
        use super::mailbox_evidence::{FactTone, mailbox_evidence_card};
        let card = mailbox_evidence_card(mailbox);
        let colors = self.theme_colors();
        let tone_color = |tone: FactTone| match tone {
            FactTone::Good => colors.success,
            FactTone::Warning => colors.warning,
            FactTone::Bad => colors.danger,
            FactTone::Neutral => colors.text_secondary,
        };
        let language = self.language;
        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            crate::ui::section_label(ui, language.message("ui.mailbox-migration-evidence"));
            ui.columns(card.facts.len(), |columns| {
                for (column, fact) in columns.iter_mut().zip(card.facts.iter()) {
                    column.label(RichText::new(language.message(fact.label)).strong());
                    let status = match &fact.detail {
                        Some(detail) => format!("{} ({detail})", language.message(fact.status)),
                        None => language.message(fact.status).to_owned(),
                    };
                    column.label(RichText::new(status).color(tone_color(fact.tone)));
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new(language.message("ui.operator-conclusion")).strong());
            ui.label(
                RichText::new(language.message(card.conclusion))
                    .color(tone_color(card.conclusion_tone)),
            );
        });
    }

    /// Folder summary, filters, a page of individual differences, and CSV
    /// export for one mailbox's evidence run.
    pub(crate) fn mismatch_inspector_ui(
        &mut self,
        ui: &mut egui::Ui,
        job_id: &str,
        run_id: &str,
        evidence: &MailboxEvidence,
    ) {
        let language = self.language;
        let colors = self.theme_colors();
        egui::CollapsingHeader::new(language.message("ui.inspect-differences"))
            .id_salt(("mismatch_inspector", job_id))
            .show(ui, |ui| {
                self.mismatch_inspector.select(job_id, run_id);
                if self.mismatch_inspector.stale {
                    self.mismatch_inspector.load(&self.store);
                }
                // Messages the verifier could not settle are not stored per
                // message; state how many there are rather than hiding them.
                let unflagged = evidence.flag_verification.map(|flags| {
                    evidence
                        .source_messages
                        .saturating_sub(flags.compared_messages)
                });
                ui.label(
                    RichText::new(
                        language
                            .message("ui.unverifiable-summary")
                            .replace("{probable}", &evidence.probable_count().to_string())
                            .replace(
                                "{flags}",
                                &unflagged.map_or_else(
                                    || language.message("ui.evidence-not-checked").to_owned(),
                                    |count| count.to_string(),
                                ),
                            ),
                    )
                    .color(colors.text_secondary),
                );
                if let Some(error) = &self.mismatch_inspector.error {
                    ui.label(RichText::new(error).color(colors.danger));
                    return;
                }
                let folders = folder_rows(&self.mismatch_inspector.summary);
                if folders.is_empty() {
                    ui.label(language.message("ui.no-message-differences-recorded"));
                    return;
                }
                ui.label(
                    RichText::new(language.message("ui.folder-names-not-stored"))
                        .color(colors.text_secondary),
                );
                let unknown = language.message("ui.unknown-folder");
                let mut filter = self.mismatch_inspector.filter.clone();
                egui::Grid::new(("mismatch_folders", job_id))
                    .striped(true)
                    .num_columns(TYPES.len() + 1)
                    .show(ui, |ui| {
                        let all = ui.selectable_label(
                            filter.folder_digest.is_none(),
                            RichText::new(language.message("ui.all-folders")).strong(),
                        );
                        crate::ui::name_control(&all, language.message("ui.all-folders"));
                        if all.clicked() {
                            filter.folder_digest = None;
                        }
                        for mismatch_type in TYPES {
                            ui.label(
                                RichText::new(language.message(type_key(mismatch_type))).strong(),
                            );
                        }
                        ui.end_row();
                        for (digest, counts) in &folders {
                            let label = self.observed_folder_names.label(Some(digest), unknown);
                            let response = ui.selectable_label(
                                filter.folder_digest.as_deref() == Some(digest.as_str()),
                                &label,
                            );
                            crate::ui::name_control(&response, &label);
                            if response.clicked() {
                                filter.folder_digest = Some(digest.clone());
                            }
                            for count in counts {
                                ui.label(count.to_string());
                            }
                            ui.end_row();
                        }
                    });
                let mut export = false;
                ui.horizontal(|ui| {
                    let selected = filter
                        .mismatch_type
                        .map_or(language.message("ui.all-difference-types"), |value| {
                            language.message(type_key(value))
                        });
                    let combo = egui::ComboBox::from_id_salt(("mismatch_type", job_id))
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut filter.mismatch_type,
                                None,
                                language.message("ui.all-difference-types"),
                            );
                            for mismatch_type in TYPES {
                                ui.selectable_value(
                                    &mut filter.mismatch_type,
                                    Some(mismatch_type),
                                    language.message(type_key(mismatch_type)),
                                );
                            }
                        });
                    crate::ui::name_control(
                        &combo.response,
                        language.message("ui.difference-type-filter"),
                    );
                    export = ui
                        .button(language.message("ui.export-differences-csv"))
                        .clicked();
                });
                if export {
                    let result = self.export_mismatches_csv(job_id, run_id, &filter);
                    self.report_export_result("Verification differences", result);
                }
                self.mismatch_inspector.set_filter(filter);
                if self.mismatch_inspector.stale {
                    self.mismatch_inspector.load(&self.store);
                }
                egui::Grid::new(("mismatch_rows", job_id))
                    .striped(true)
                    .num_columns(5)
                    .show(ui, |ui| {
                        for key in [
                            "ui.difference-type",
                            "ui.folders",
                            "ui.source-uid",
                            "ui.destination-uid",
                            "ui.size-and-date",
                        ] {
                            ui.label(RichText::new(language.message(key)).strong());
                        }
                        ui.end_row();
                        for mismatch in &self.mismatch_inspector.rows {
                            ui.label(language.message(type_key(mismatch.mismatch_type)));
                            ui.label(folder_text(&self.observed_folder_names, mismatch, unknown));
                            ui.label(uid_text(mismatch.source_uidvalidity, &mismatch.source_uid));
                            ui.label(uid_text(
                                mismatch.destination_uidvalidity,
                                &mismatch.dest_uid,
                            ));
                            ui.label(size_and_date_text(mismatch));
                            ui.end_row();
                        }
                    });
                let inspector = &mut self.mismatch_inspector;
                ui.horizontal(|ui| {
                    let previous = ui.add_enabled(
                        !inspector.previous.is_empty(),
                        egui::Button::new(language.message("ui.previous")),
                    );
                    let next = ui.add_enabled(
                        inspector.more,
                        egui::Button::new(language.message("ui.next")),
                    );
                    if previous.clicked() {
                        inspector.after = inspector.previous.pop().unwrap_or(0);
                        inspector.stale = true;
                    } else if next.clicked()
                        && let Some(last) = inspector.rows.last()
                    {
                        inspector.previous.push(inspector.after);
                        inspector.after = last.key;
                        inspector.stale = true;
                    }
                });
            });
    }

    fn export_mismatches_csv(
        &self,
        job_id: &str,
        run_id: &str,
        filter: &MismatchFilter,
    ) -> Result<(), String> {
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-verification-differences.csv")
            .save_file()
            .ok_or("Export cancelled.")?;
        let (csv, _, truncated) = self.store.export_message_mismatches_csv(
            job_id,
            run_id,
            filter,
            MAX_EXPORT_ROWS,
            &|digest| self.observed_folder_names.name(digest).map(str::to_owned),
        )?;
        if truncated {
            return Err(format!(
                "more than {MAX_EXPORT_ROWS} differences match; narrow the filter and export again"
            ));
        }
        crate::atomic_artifact::write_private_atomic(&path, &csv).map_err(|error| error.to_string())
    }
}

fn folder_text(names: &ObservedFolderNames, mismatch: &StoredMismatch, unknown: &str) -> String {
    match (
        mismatch.source_folder_digest.as_deref(),
        mismatch.destination_folder_digest.as_deref(),
    ) {
        (Some(source), Some(destination)) if source != destination => format!(
            "{} → {}",
            names.label(Some(source), unknown),
            names.label(Some(destination), unknown)
        ),
        (Some(folder), _) | (None, Some(folder)) => names.label(Some(folder), unknown),
        (None, None) => names.label(None, unknown),
    }
}

fn uid_text(uidvalidity: Option<u64>, uid: &Option<String>) -> String {
    match (uidvalidity, uid) {
        (Some(uidvalidity), Some(uid)) => format!("{uid} (UIDVALIDITY {uidvalidity})"),
        (None, Some(uid)) => uid.clone(),
        _ => "—".to_owned(),
    }
}

fn size_and_date_text(mismatch: &StoredMismatch) -> String {
    let side = |size: Option<u64>, date: &Option<String>| match (size, date) {
        (None, None) => "—".to_owned(),
        (size, date) => format!(
            "{} {}",
            size.map_or_else(|| "?".to_owned(), |size| size.to_string()),
            date.as_deref().unwrap_or("")
        )
        .trim()
        .to_owned(),
    };
    format!(
        "{} → {}",
        side(mismatch.source_size_bytes, &mismatch.source_date),
        side(mismatch.dest_size_bytes, &mismatch.dest_date)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_rows_place_each_type_in_its_column() {
        let entry = |folder: &str, mismatch_type, count| MismatchFolderSummary {
            folder_digest: folder.to_owned(),
            mismatch_type,
            count,
        };
        let rows = folder_rows(&[
            entry("aa", MismatchType::Extra, 2),
            entry("bb", MismatchType::Missing, 3),
            entry("bb", MismatchType::PresentWrongFolder, 1),
        ]);
        assert_eq!(
            rows,
            vec![
                ("aa".to_owned(), [0, 2, 0, 0, 0]),
                ("bb".to_owned(), [3, 0, 0, 1, 0]),
            ]
        );
    }

    #[test]
    fn folder_digests_show_names_only_when_observed_in_this_process() {
        let mut names = ObservedFolderNames::default();
        let digest = crate::core::folder_digest("project", "Sent Items");
        assert_eq!(
            names.label(Some(&digest), "unknown"),
            format!("#{}", &digest[..12])
        );
        let mismatch = MessageMismatch {
            id: "m".into(),
            job_id: "job".into(),
            run_id: "run".into(),
            mismatch_type: MismatchType::Missing,
            source_folder: Some("Sent Items".into()),
            destination_folder: None,
            source_uidvalidity: None,
            destination_uidvalidity: None,
            source_uid: None,
            dest_uid: None,
            source_message_id: None,
            dest_message_id: None,
            source_size_bytes: None,
            dest_size_bytes: None,
            source_date: None,
            dest_date: None,
            source_fingerprint: None,
            destination_fingerprint: None,
        };
        names.observe("project", std::slice::from_ref(&mismatch));
        assert_eq!(names.label(Some(&digest), "unknown"), "Sent Items");
        // Digests are project-scoped.
        let other = crate::core::folder_digest("other", "Sent Items");
        assert_ne!(names.label(Some(&other), "unknown"), "Sent Items");
        assert_eq!(names.label(Some(""), "unknown"), "unknown");
    }
}
