use super::{pick_account, App};
use balance::inventory::InvoiceKind;
use balance::opening_bills::{label, OpeningBillDraft};
use balance::{parse_amount, Category, Error, Ledger, Result};
use eframe::egui;

pub(super) struct OpeningBillForm {
    kind: InvoiceKind,
    party: Option<i64>,
    reference: String,
    original_date: String,
    opening_date: String,
    due_date: String,
    amount: String,
}

impl Default for OpeningBillForm {
    fn default() -> Self {
        Self {
            kind: InvoiceKind::Sale,
            party: None,
            reference: String::new(),
            original_date: String::new(),
            opening_date: String::new(),
            due_date: String::new(),
            amount: String::new(),
        }
    }
}

impl App {
    pub(super) fn opening_bill_panel(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        ledgers: &[Ledger],
    ) {
        ui.collapsing("Opening receivables and payables", |ui| {
            if let Err(error) = self.opening_bill_form(ui, company, ledgers) {
                self.failure(error);
            }
        });
    }

    fn opening_bill_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        ledgers: &[Ledger],
    ) -> Result<()> {
        ui.small("Enter only the amount still owed when the books open. Use opening bills for this ledger in place of an aggregate opening balance.");
        let form = &mut self.opening_bill;
        let initial = form.kind;
        egui::ComboBox::from_id_salt("opening-bill-kind")
            .selected_text(label(form.kind))
            .show_ui(ui, |ui| {
                for kind in InvoiceKind::ALL {
                    ui.selectable_value(&mut form.kind, kind, label(kind));
                }
            });
        if initial != form.kind {
            form.party = None;
        }
        let category = match form.kind {
            InvoiceKind::Sale => Category::Asset,
            InvoiceKind::Purchase => Category::Liability,
        };
        let parties = ledgers
            .iter()
            .filter(|ledger| ledger.category == category)
            .cloned()
            .collect::<Vec<_>>();
        pick_account(
            ui,
            "opening-bill-party",
            "Party",
            &parties,
            &mut form.party,
            false,
        );
        egui::Grid::new("opening-bill-fields").show(ui, |ui| {
            for (label, value) in [
                ("Original bill reference", &mut form.reference),
                ("Original bill date (YYYY-MM-DD)", &mut form.original_date),
                ("Opening date (YYYY-MM-DD)", &mut form.opening_date),
                ("Due date (optional)", &mut form.due_date),
                ("Outstanding amount", &mut form.amount),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        if ui.button("Post opening bill").clicked() {
            let draft = OpeningBillDraft {
                kind: form.kind,
                party_ledger_id: form
                    .party
                    .ok_or_else(|| Error::Validation("Select the party ledger".into()))?,
                reference: form.reference.clone(),
                original_date: form.original_date.clone(),
                opening_date: form.opening_date.clone(),
                due_date: (!form.due_date.trim().is_empty())
                    .then(|| form.due_date.trim().to_owned()),
                credit_days: None,
                amount: parse_amount(&form.amount)?,
            };
            let id = self.store.post_opening_bill(company, &draft)?;
            form.reference.clear();
            form.original_date.clear();
            form.due_date.clear();
            form.amount.clear();
            self.message =
                format!("Opening bill {id} posted. Find it under Invoices or Outstanding.");
        }
        Ok(())
    }
}
