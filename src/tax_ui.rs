use super::{money, pick_account, App};
use balance::inventory::InvoiceKind;
use balance::taxes::InvoiceTaxRegister;
use balance::Category;
use eframe::egui;

#[derive(Default)]
pub(super) struct TaxRegisterScreen {
    from: String,
    to: String,
    kind: Option<InvoiceKind>,
    ledger: Option<i64>,
    report: Option<InvoiceTaxRegister>,
}

impl App {
    pub(super) fn tax_register_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Invoice tax register");
        ui.small("Recorded invoice tax and full reversals. Dates apply separately to invoices and notes.");
        self.report_export_path_field(ui);
        let ledgers = match self.store.ledgers(company) {
            Ok(rows) => rows
                .into_iter()
                .filter(|row| matches!(row.category, Category::Asset | Category::Liability))
                .collect::<Vec<_>>(),
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let form = &mut self.tax_register_screen;
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            for (label, input) in [("From", &mut form.from), ("Through", &mut form.to)] {
                ui.label(label);
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(input)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(105.0),
                    )
                    .changed();
            }
            let previous = form.kind;
            egui::ComboBox::from_id_salt("tax-invoice-kind")
                .selected_text(form.kind.map(InvoiceKind::as_str).unwrap_or("All types"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.kind, None, "All types");
                    for kind in InvoiceKind::ALL {
                        ui.selectable_value(&mut form.kind, Some(kind), kind.as_str());
                    }
                });
            changed |= form.kind != previous;
        });
        let prev_ledger = form.ledger;
        pick_account(
            ui,
            "tax-register-ledger",
            "Ledger (optional)",
            &ledgers,
            &mut form.ledger,
            true,
        );
        if changed || form.ledger != prev_ledger {
            form.report = None;
        }
        let from = form.from.trim();
        let to = form.to.trim();
        let from = (!from.is_empty()).then_some(from);
        let to = (!to.is_empty()).then_some(to);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Run register").clicked() {
                match self
                    .store
                    .invoice_tax_register(company, from, to, form.kind, form.ledger)
                {
                    Ok(report) => {
                        form.report = Some(report);
                        self.message.clear();
                    }
                    Err(error) => {
                        form.report = None;
                        self.message = error.to_string();
                    }
                }
            }
            if ui.button("Export tax register CSV").clicked() {
                match self.store.export_invoice_tax_register_csv(
                    company,
                    from,
                    to,
                    form.kind,
                    form.ledger,
                    self.report_export_path.trim(),
                ) {
                    Ok(()) => self.message = "Invoice tax register exported".into(),
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
        let Some(report) = &form.report else { return };
        ui.label(format!(
            "Sales tax {} | Purchase tax {}",
            money(report.sales),
            money(report.purchases)
        ));
        if report.entries.is_empty() {
            ui.label("No recorded invoice taxes match these filters.");
        }
        egui::CollapsingHeader::new("Tax by ledger")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("tax-by-ledger")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in ["Tax ledger", "Sales tax", "Purchase tax"] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for row in &report.ledgers {
                            ui.label(&row.ledger);
                            ui.label(money(row.sales));
                            ui.label(money(row.purchases));
                            ui.end_row();
                        }
                    });
            });
        egui::ScrollArea::both()
            .id_salt("invoice-tax-detail")
            .max_height(420.0)
            .show(ui, |ui| {
                egui::Grid::new("invoice-tax-entries")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in [
                            "Date",
                            "Type",
                            "Document",
                            "Reference",
                            "Party",
                            "Tax ledger",
                            "Amount",
                        ] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for row in &report.entries {
                            ui.label(row.date.to_string());
                            ui.label(row.kind.as_str());
                            ui.label(if row.reversal { "Reversal" } else { "Invoice" });
                            ui.label(&row.reference);
                            ui.label(&row.party);
                            ui.label(&row.ledger);
                            ui.label(money(row.amount));
                            ui.end_row();
                        }
                    });
            });
    }
}
