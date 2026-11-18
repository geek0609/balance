use super::{money, App};
use balance::actual_cost::{ActualCostIssue, AverageCostValuation, StockReceipt};
use balance::inventory::format_quantity;
use balance::parse_amount;
use eframe::egui;

#[derive(Default)]
pub(super) struct ActualCostScreen {
    as_of: String,
    report: Option<AverageCostValuation>,
    receipt_search: String,
    selected: Option<StockReceipt>,
    amount: String,
    reason: String,
}
impl ActualCostScreen {
    fn select(&mut self, receipt: StockReceipt) {
        self.amount = receipt.cost.map(balance::format_amount).unwrap_or_default();
        self.reason.clear();
        self.selected = Some(receipt);
    }
}

impl App {
    pub(super) fn average_cost_view(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.small("Net purchase amounts and recorded acquisition costs determine stock value across all locations.");
        egui::CollapsingHeader::new("Acquisition costs for stock adjustments")
            .open(self.actual_cost_screen.selected.is_some().then_some(true))
            .show(ui, |ui| {
                self.receipt_cost_form(ui, company);
            });
        let mut run = false;
        let mut export = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("As of");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.actual_cost_screen.as_of)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(110.0),
                )
                .changed()
            {
                self.actual_cost_screen.report = None;
            }
            run = ui.button("Run weighted average valuation").clicked();
            export = ui.button("Export weighted average CSV").clicked();
        });
        let date = self.actual_cost_screen.as_of.trim();
        if run {
            match self.store.stock_valuation_at_average_cost(company, date) {
                Ok(report) => {
                    self.actual_cost_screen.report = Some(report);
                    self.message.clear();
                }
                Err(error) => {
                    self.actual_cost_screen.report = None;
                    self.message = error.to_string();
                }
            }
        }
        if export {
            match self.store.export_average_stock_valuation_csv(
                company,
                date,
                self.report_export_path.trim(),
            ) {
                Ok(()) => self.message = "Weighted average valuation exported".into(),
                Err(error) => self.message = error.to_string(),
            }
        }
        self.actual_value_table(ui, company);
    }

    fn actual_value_table(&mut self, ui: &mut egui::Ui, company: i64) {
        let Some(report) = &self.actual_cost_screen.report else {
            return;
        };
        ui.label(format!(
            "{} | Known stock value {} | {} items unavailable",
            report.date,
            money(report.known_value),
            report.unavailable
        ));
        let mut edit_receipt = None;
        egui::ScrollArea::both()
            .id_salt("average-stock-table")
            .max_height(410.0)
            .show(ui, |ui| {
                egui::Grid::new("average-stock-valuation")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in ["Item", "Quantity", "Stock value", "Issue"] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for row in &report.rows {
                            ui.label(&row.item.name);
                            ui.label(format!(
                                "{} {}",
                                format_quantity(row.quantity),
                                row.item.unit
                            ));
                            ui.label(row.value.map(money).unwrap_or_else(|| "Unavailable".into()));
                            if let Some(issue) = &row.issue {
                                if let ActualCostIssue::MissingReceiptCost { receipt_id, .. } =
                                    issue
                                {
                                    if ui.link(issue.to_string()).clicked() {
                                        edit_receipt = Some(*receipt_id);
                                    }
                                } else {
                                    ui.label(issue.to_string());
                                }
                            } else {
                                ui.label("—");
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(id) = edit_receipt {
            match self.store.stock_receipts(company, Some(id)) {
                Ok(mut receipts) => {
                    if let Some(receipt) = receipts.pop() {
                        self.actual_cost_screen.select(receipt);
                    }
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn receipt_cost_form(&mut self, ui: &mut egui::Ui, company: i64) {
        if self.actual_cost_screen.selected.is_none() {
            ui.add(
                egui::TextEdit::singleline(&mut self.actual_cost_screen.receipt_search)
                    .hint_text("Adjustment ID (optional)"),
            );
            let raw = self.actual_cost_screen.receipt_search.trim();
            let id = if raw.is_empty() {
                None
            } else if let Ok(id) = raw.parse::<i64>() {
                Some(id)
            } else {
                ui.label("Enter the adjustment ID as a number.");
                return;
            };
            match self.store.stock_receipts(company, id) {
                Ok(receipts) => {
                    if receipts.is_empty() {
                        ui.label("No matching quantity increases.");
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("stock-receipts")
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for receipt in receipts {
                                if ui
                                    .selectable_label(false, receipt_label(&receipt))
                                    .clicked()
                                {
                                    self.actual_cost_screen.select(receipt);
                                }
                            }
                        });
                }
                Err(error) => self.failure(error),
            }
        }
        let Some(receipt) = &self.actual_cost_screen.selected else {
            return;
        };
        let id = receipt.id;
        ui.label(receipt_label(receipt));
        if !receipt.description.is_empty() {
            ui.label(&receipt.description);
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Total acquisition cost");
            ui.text_edit_singleline(&mut self.actual_cost_screen.amount);
            ui.label("Reason");
            ui.text_edit_singleline(&mut self.actual_cost_screen.reason);
        });
        let mut save = false;
        let mut dismiss = false;
        ui.horizontal(|ui| {
            save = ui.button("Save acquisition cost").clicked();
            dismiss = ui.button("Back to receipts").clicked();
        });
        if dismiss {
            self.actual_cost_screen.selected = None;
            return;
        }
        if save {
            let result = parse_amount(&self.actual_cost_screen.amount).and_then(|amount| {
                self.store.set_stock_receipt_cost(
                    company,
                    id,
                    amount,
                    &self.actual_cost_screen.reason,
                )?;
                Ok(amount)
            });
            match result {
                Ok(amount) => {
                    if let Some(receipt) = &mut self.actual_cost_screen.selected {
                        receipt.cost = Some(amount);
                    }
                    self.actual_cost_screen.reason.clear();
                    self.actual_cost_screen.report = None;
                    self.message = "Acquisition cost saved".into();
                }
                Err(error) => self.failure(error),
            }
        }
        egui::CollapsingHeader::new("Acquisition cost history")
            .id_salt(id)
            .show(ui, |ui| {
                match self.store.stock_receipt_cost_history(company, id) {
                    Ok(entries) if entries.is_empty() => {
                        ui.label("No acquisition cost saved.");
                    }
                    Ok(entries) => {
                        for entry in entries {
                            ui.label(format!(
                                "{} UTC | {} | {}",
                                entry.recorded_at,
                                money(entry.amount.into()),
                                entry.reason
                            ));
                        }
                    }
                    Err(error) => self.failure(error),
                }
            });
    }
}

fn receipt_label(receipt: &StockReceipt) -> String {
    let mut label = format!(
        "{} | Receipt {} | {} | {} | {} {}",
        receipt.date,
        receipt.id,
        receipt.item,
        receipt.location,
        format_quantity(receipt.quantity.into()),
        receipt.unit
    );
    if let Some(batch) = &receipt.batch {
        label.push_str(&format!(" | Batch {batch}"));
    }
    label.push_str(" | ");
    if let Some(cost) = receipt.cost {
        label.push_str(&money(cost.into()));
    } else {
        label.push_str("Missing cost");
    }
    label
}
