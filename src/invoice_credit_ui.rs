use crate::{money, App};
use balance::inventory::{InvoiceKind, InvoiceSummary};
use balance::invoice_credits::{CreditComponent, CreditLineDraft};
use balance::{parse_amount, Result};
use eframe::egui;

pub(crate) struct NoteForm {
    invoice: i64,
    reversal: Option<i64>,
    date: String,
    reference: String,
    reason: String,
    lines: Vec<(CreditComponent, String)>,
}

impl NoteForm {
    fn new(invoice: i64, reversal: Option<i64>, components: Vec<CreditComponent>) -> Self {
        Self {
            invoice,
            reversal,
            date: String::new(),
            reference: String::new(),
            reason: String::new(),
            lines: components
                .into_iter()
                .map(|part| (part, String::new()))
                .collect(),
        }
    }

    fn amounts(&self) -> Result<Vec<CreditLineDraft>> {
        self.lines
            .iter()
            .filter(|(_, value)| !value.trim().is_empty())
            .map(|(part, value)| {
                Ok(CreditLineDraft {
                    ledger_id: part.ledger_id,
                    is_tax: part.is_tax,
                    amount: parse_amount(value)?,
                })
            })
            .collect()
    }
}

impl App {
    pub(crate) fn invoice_credit_panel(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        invoice: &InvoiceSummary,
    ) {
        if invoice.opening_bill.is_some() {
            return;
        }
        let label = match invoice.kind {
            InvoiceKind::Sale => "credit note",
            InvoiceKind::Purchase => "debit note",
        };
        if !invoice.has_credit_history && (invoice.outstanding == 0 || invoice.reversal.is_some()) {
            return;
        }
        egui::CollapsingHeader::new(format!("Partial {label}s"))
            .id_salt(("invoice-credits", invoice.id))
            .show(ui, |ui| {
                let notes = match self.store.invoice_credit_notes(company, invoice.id) {
                    Ok(notes) => notes,
                    Err(e) => { self.failure(e); return; }
                };
                for note in notes {
                    ui.push_id(note.id, |ui| {
                        ui.label(format!("{} · {} · {} · Voucher {} · {}", note.date, note.reference, money(i128::from(note.amount)), note.voucher_id, note.reason));
                        if let Some(voucher) = note.reversal_voucher_id {
                            ui.label(format!("Reversed {} · Voucher {} · {}", note.reversal_date.as_deref().unwrap_or_default(), voucher, note.reversal_reason.as_deref().unwrap_or_default()));
                        } else if ui.add_enabled(self.invoice_credit.is_none(), egui::Button::new("Reverse note")).clicked() {
                            self.invoice_credit = Some(NoteForm::new(invoice.id, Some(note.id), Vec::new()));
                        }
                    });
                }
                if ui.add_enabled(
                    self.invoice_credit.is_none() && invoice.outstanding > 0 && invoice.reversal.is_none() && self.editing_invoice != Some(invoice.id),
                    egui::Button::new(format!("Add partial {label}")),
                ).clicked() {
                    match self.store.invoice_credit_components(company, invoice.id) {
                        Ok(components) => self.invoice_credit = Some(NoteForm::new(invoice.id, None, components)),
                        Err(e) => self.failure(e),
                    }
                }
                let Some(form) = self.invoice_credit.as_mut().filter(|form| form.invoice == invoice.id) else { return };
                if form.reversal.is_none() {
                    ui.small(format!("Unpaid balance {}. Use the original ledger components to reduce this bill. Enter positive amounts for the components to adjust. Stock quantities stay as posted.", money(invoice.outstanding)));
                    egui::Grid::new(("note-components", invoice.id)).show(ui, |ui| {
                        for heading in ["Component", "Remaining", "Amount"] {
                            ui.strong(heading);
                        }
                        ui.end_row();
                        for (part, value) in &mut form.lines {
                            let category = if part.is_tax { " (tax)" } else if part.remaining < 0 { " (deduction)" } else { "" };
                            ui.label(format!("{}{category}", part.ledger));
                            ui.label(money(part.remaining));
                            ui.add(egui::TextEdit::singleline(value).desired_width(90.0));
                            ui.end_row();
                        }
                    });
                }
                egui::Grid::new(("note-fields", invoice.id)).show(ui, |ui| {
                    for (label, value) in [("Date (YYYY-MM-DD)", &mut form.date), ("Reference", &mut form.reference), ("Reason", &mut form.reason)] {
                        ui.label(label);
                        ui.text_edit_singleline(value);
                        ui.end_row();
                    }
                });
                let (save, cancel) = ui.horizontal(|ui| {
                    let save = ui.button(if form.reversal.is_some() { "Post reversal" } else { "Post note" }).clicked();
                    (save, ui.button("Cancel").clicked())
                }).inner;
                if cancel {
                    self.invoice_credit = None;
                } else if save {
                    let result = if let Some(note) = form.reversal {
                        self.store.reverse_invoice_credit(company, note, &form.date, &form.reference, &form.reason)
                            .map(|id| format!("Note reversal voucher {id} posted"))
                    } else {
                        form.amounts().and_then(|lines| self.store.post_invoice_credit(company, invoice.id, &form.date, &form.reference, &form.reason, &lines))
                            .map(|id| format!("Invoice note {id} posted"))
                    };
                    match result {
                        Ok(message) => { self.message = message; self.invoice_credit = None; }
                        Err(e) => self.failure(e),
                    }
                }
            });
    }
}
