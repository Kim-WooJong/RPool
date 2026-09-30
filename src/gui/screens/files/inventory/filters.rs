use super::state::{CodingFilter, InventoryForm};
use crate::gui::i18n::tr;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, form: &mut InventoryForm) {
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.query)
                .hint_text(tr("Search files, archive IDs, remotes…"))
                .desired_width(280.0),
        );

        egui::ComboBox::from_id_salt("inventory-coding-filter")
            .selected_text(form.coding_filter.label())
            .show_ui(ui, |ui| {
                for filter in [
                    CodingFilter::All,
                    CodingFilter::ReedSolomon,
                    CodingFilter::Plain,
                ] {
                    ui.selectable_value(&mut form.coding_filter, filter, filter.label());
                }
            });

        if (!form.query.is_empty() || form.coding_filter != CodingFilter::All)
            && ui.button(tr("Clear")).clicked()
        {
            form.query.clear();
            form.coding_filter = CodingFilter::All;
        }
    });
}
