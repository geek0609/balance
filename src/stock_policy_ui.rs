use super::App;
use balance::Result;
use eframe::egui;

#[derive(Default)]
pub(super) struct NegativeStockScreen {
    from: String,
    reason: String,
}

impl App {
    pub(super) fn negative_stock_view(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        let current = self.store.negative_stock_blocked_from(company)?;
        if let Some(date) = current {
            ui.label(format!("Closing balances must be nonnegative from {date}"));
        } else {
            ui.label("Negative closing balances are allowed");
        }
        let form = &mut self.negative_stock_screen;
        egui::Grid::new("negative-stock-settings").show(ui, |ui| {
            ui.label("Check balances from");
            ui.add(egui::TextEdit::singleline(&mut form.from).hint_text("YYYY-MM-DD"));
            ui.end_row();
            ui.label("Reason");
            ui.text_edit_singleline(&mut form.reason);
            ui.end_row();
        });
        let (enable, disable) = ui
            .horizontal_wrapped(|ui| {
                let has_reason = !form.reason.trim().is_empty();
                (
                    ui.add_enabled(
                        has_reason && !form.from.trim().is_empty(),
                        egui::Button::new("Block negative stock"),
                    )
                    .clicked(),
                    ui.add_enabled(
                        has_reason && current.is_some(),
                        egui::Button::new("Allow negative stock"),
                    )
                    .clicked(),
                )
            })
            .inner;
        if enable || disable {
            self.store.set_negative_stock_policy(
                company,
                enable.then_some(form.from.trim()),
                &form.reason,
            )?;
            *form = NegativeStockScreen::default();
            self.message = "Negative stock policy saved".into();
        }
        egui::CollapsingHeader::new("Policy history")
            .show(ui, |ui| -> Result<()> {
                for change in self.store.negative_stock_policy_history(company)? {
                    let setting = change
                        .blocked_from
                        .map(|date| format!("Blocked from {date}"))
                        .unwrap_or_else(|| "Allowed".into());
                    ui.label(format!(
                        "{} UTC · {setting} · {}",
                        change.changed_at, change.reason
                    ));
                }
                Ok(())
            })
            .body_returned
            .transpose()?;
        Ok(())
    }
}
