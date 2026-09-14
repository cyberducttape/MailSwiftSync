//! Worksheet selection for batch imports.

use crate::App;
use eframe::egui;
use eframe::egui::RichText;

impl App {
    pub(crate) fn bulk_sheet_selection(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_sheet_import.as_ref() else {
            return;
        };
        let path_label = pending
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("the workbook")
            .to_owned();
        let sheets = pending.sheets.clone();
        let mut open = true;
        let mut cancel = false;
        let mut import = false;
        egui::Window::new("Choose worksheet")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Select the migration worksheet");
                ui.label(format!(
                    "{path_label} contains {} worksheet(s). Choose the sheet with the mailbox headers.",
                    sheets.len()
                ));
                egui::ComboBox::from_id_salt("bulk_sheet_selection")
                    .selected_text(
                        sheets
                            .get(self.bulk_sheet_index)
                            .map(String::as_str)
                            .unwrap_or("Select a worksheet"),
                    )
                    .show_ui(ui, |ui| {
                        for (index, name) in sheets.iter().enumerate() {
                            ui.selectable_value(&mut self.bulk_sheet_index, index, name);
                        }
                    });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "The selected worksheet is parsed and validated in the background. Other worksheets are not imported.",
                    )
                    .size(11.0)
                    .color(self.theme_colors().text_secondary),
                );
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Import selected worksheet").clicked() {
                        import = true;
                    }
                });
            });
        if cancel || !open {
            self.pending_sheet_import = None;
            self.bulk_message = "Worksheet selection cancelled; no rows were imported.".into();
        } else if import && let Some(pending) = self.pending_sheet_import.take() {
            self.begin_sheet_import(pending.path, self.bulk_sheet_index);
        }
    }
}
