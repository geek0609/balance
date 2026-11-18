use super::{money, pick_account, App};
use balance::advances::{AdvanceDraft, PartyAdvance};
use balance::inventory::InvoiceKind;
use balance::{parse_amount, Category, Error, Result};
use eframe::egui;

struct AdvanceEntry {
    kind: InvoiceKind,
    party: Option<i64>,
    bank: Option<i64>,
    date: String,
    reference: String,
    amount: String,
    narration: String,
}
impl Default for AdvanceEntry {
    fn default() -> Self {
        Self {
            kind: InvoiceKind::Sale,
            party: None,
            bank: None,
            date: String::new(),
            reference: String::new(),
            amount: String::new(),
            narration: String::new(),
        }
    }
}
impl AdvanceEntry {
    fn draft(&self) -> Result<AdvanceDraft> {
        Ok(AdvanceDraft {
            kind: self.kind,
            party_ledger_id: self
                .party
                .ok_or_else(|| Error::Validation("Select the party ledger".into()))?,
            bank_ledger_id: self
                .bank
                .ok_or_else(|| Error::Validation("Select the cash or bank ledger".into()))?,
            date: self.date.clone(),
            reference: self.reference.clone(),
            amount: parse_amount(&self.amount)?,
            narration: self.narration.clone(),
        })
    }
}

#[derive(Default)]
struct ReversalForm {
    date: String,
    reference: String,
    reason: String,
}

#[derive(Default)]
pub(super) struct AdvanceScreen {
    entry: AdvanceEntry,
    selected: Option<i64>,
    reversal: Option<ReversalForm>,
    search: String,
    as_of: String,
    kind: Option<InvoiceKind>,
}

impl App {
    pub(super) fn advances_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Customer and supplier advances");
        egui::CollapsingHeader::new("Record advance")
            .default_open(true)
            .show(ui, |ui| {
                self.advance_entry_form(ui, company);
            });
        ui.separator();
        ui.heading("Advance register");
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("advance-kind-filter")
                .selected_text(
                    self.advance_screen
                        .kind
                        .map(kind_label)
                        .unwrap_or("All types"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.advance_screen.kind, None, "All types");
                    for kind in InvoiceKind::ALL {
                        ui.selectable_value(
                            &mut self.advance_screen.kind,
                            Some(kind),
                            kind_label(kind),
                        );
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.advance_screen.search)
                    .hint_text("Party or reference"),
            );
            ui.label("As of");
            ui.add(
                egui::TextEdit::singleline(&mut self.advance_screen.as_of)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(110.0),
            );
        });
        let date = self.advance_screen.as_of.trim();
        let advances = match self
            .store
            .party_advances(company, (!date.is_empty()).then_some(date))
        {
            Ok(advances) => advances,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let search = self.advance_screen.search.trim().to_lowercase();
        let mut found = false;
        for advance in advances {
            if self
                .advance_screen
                .kind
                .is_some_and(|kind| kind != advance.kind)
                || (!search.is_empty()
                    && !advance.party.to_lowercase().contains(&search)
                    && !advance.reference.to_lowercase().contains(&search))
            {
                continue;
            }
            found = true;
            let current = self.advance_screen.selected == Some(advance.id);
            let status = if advance.reversal.is_some() {
                " | Reversed"
            } else {
                ""
            };
            if ui
                .selectable_label(
                    current,
                    format!(
                        "{} | {} | {} | {} | {}{status}",
                        advance.date,
                        kind_label(advance.kind),
                        advance.party,
                        advance.reference,
                        money(advance.amount.into())
                    ),
                )
                .clicked()
            {
                self.advance_screen.selected = if current { None } else { Some(advance.id) };
                self.advance_screen.reversal = None;
            }
            if self.advance_screen.selected == Some(advance.id) {
                self.advance_details(ui, company, &advance);
            }
        }
        if !found {
            ui.label("No matching advances.");
        }
    }

    fn advance_entry_form(&mut self, ui: &mut egui::Ui, company: i64) {
        let ledgers = match self.store.ledgers(company) {
            Ok(ledgers) => ledgers,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let banks = ledgers
            .iter()
            .filter(|ledger| ledger.category == Category::Asset)
            .cloned()
            .collect::<Vec<_>>();
        let parties = ledgers
            .into_iter()
            .filter(|ledger| matches!(ledger.category, Category::Asset | Category::Liability))
            .collect::<Vec<_>>();
        let form = &mut self.advance_screen.entry;
        egui::ComboBox::from_id_salt("advance-kind")
            .selected_text(kind_label(form.kind))
            .show_ui(ui, |ui| {
                for kind in InvoiceKind::ALL {
                    ui.selectable_value(&mut form.kind, kind, kind_label(kind));
                }
            });
        pick_account(
            ui,
            "advance-party",
            "Party",
            &parties,
            &mut form.party,
            false,
        );
        pick_account(
            ui,
            "advance-bank",
            "Cash or bank",
            &banks,
            &mut form.bank,
            false,
        );
        egui::Grid::new("advance-voucher-fields").show(ui, |ui| {
            for (label, field) in [
                ("Date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Amount", &mut form.amount),
                ("Narration", &mut form.narration),
            ] {
                ui.label(label);
                ui.text_edit_singleline(field);
                ui.end_row();
            }
        });
        if ui.button("Post advance").clicked() {
            match form
                .draft()
                .and_then(|draft| self.store.post_party_advance(company, &draft))
            {
                Ok(id) => {
                    form.reference.clear();
                    form.amount.clear();
                    form.narration.clear();
                    self.advance_screen.selected = Some(id);
                    self.advance_screen.reversal = None;
                    self.message = format!("Advance {id} posted");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn advance_details(&mut self, ui: &mut egui::Ui, company: i64, advance: &PartyAdvance) {
        ui.label(format!(
            "Advance {} | Voucher {} | Cash or bank: {}",
            advance.id, advance.voucher_id, advance.bank
        ));
        if !advance.narration.is_empty() {
            ui.label(&advance.narration);
        }
        if let Some(reversal) = &advance.reversal {
            ui.label(format!(
                "Reversed {} | {} (voucher {}) | {}",
                reversal.date, reversal.reference, reversal.voucher_id, reversal.reason
            ));
            return;
        }
        if !self.advance_screen.as_of.trim().is_empty() {
            return;
        }
        if self.advance_screen.reversal.is_none() && ui.button("Reverse advance").clicked() {
            self.advance_screen.reversal = Some(ReversalForm::default());
        }
        let Some(form) = &mut self.advance_screen.reversal else {
            return;
        };
        egui::Grid::new(("reverse-advance", advance.id)).show(ui, |ui| {
            for (label, field) in [
                ("Reversal date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(field);
                ui.end_row();
            }
        });
        let mut reverse = false;
        let mut dismiss = false;
        ui.horizontal(|ui| {
            reverse = ui.button("Post advance reversal").clicked();
            dismiss = ui.button("Cancel").clicked();
        });
        if reverse {
            match self.store.reverse_party_advance(
                company,
                advance.id,
                &form.date,
                &form.reference,
                &form.reason,
            ) {
                Ok(id) => {
                    dismiss = true;
                    self.message = format!("Advance reversed with voucher {id}");
                }
                Err(error) => self.failure(error),
            }
        }
        if dismiss {
            self.advance_screen.reversal = None;
        }
    }
}

fn kind_label(kind: InvoiceKind) -> &'static str {
    match kind {
        InvoiceKind::Sale => "Customer receipt",
        InvoiceKind::Purchase => "Supplier payment",
    }
}
