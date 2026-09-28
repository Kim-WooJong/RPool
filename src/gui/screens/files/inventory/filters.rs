use super::state::{CodingFilter, InventoryForm};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, form: &mut InventoryForm) {
    let pool_options = form.pool_options();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.query)
                .hint_text("Search files, archive IDs, remotes…")
                .desired_width(280.0),
        );

        egui::ComboBox::from_id_salt("inventory-pool-filter")
            .selected_text(if form.pool_filter.is_empty() {
                "All pools"
            } else {
                form.pool_filter.as_str()
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut form.pool_filter, String::new(), "All pools");
                for pool in pool_options {
                    ui.selectable_value(&mut form.pool_filter, pool.clone(), pool);
                }
            });

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

        if (!form.query.is_empty()
            || !form.pool_filter.is_empty()
            || form.coding_filter != CodingFilter::All)
            && ui.button("Clear").clicked()
        {
            form.query.clear();
            form.pool_filter.clear();
            form.coding_filter = CodingFilter::All;
        }
    });
}
