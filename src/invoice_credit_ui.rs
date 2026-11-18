use crate::{money, App};
use balance::inventory::{format_quantity, parse_quantity, InvoiceKind, InvoiceSummary};
use balance::invoice_credits::{CreditComponent, CreditLineDraft};
use balance::invoice_returns::{ReturnAllocation, ReturnLineDraft};
use balance::{parse_amount, Error, Result};
use eframe::egui;

pub(crate) struct NoteForm {
    invoice: i64,
    reversal: Option<i64>,
    date: String,
    reference: String,
    reason: String,
    lines: Vec<(CreditComponent, String)>,
    returns: Vec<(ReturnAllocation, String)>,
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
            returns: Vec::new(),
        }
    }

    fn quantities(&self) -> Result<Vec<ReturnLineDraft>> {
        self.returns
            .iter()
            .filter(|(_, value)| !value.trim().is_empty())
            .map(|(part, value)| {
                Ok(ReturnLineDraft {
                    allocation: part.id,
                    quantity: parse_quantity(value)?,
                })
            })
            .collect()
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
        let has_draft = self
            .invoice_credit
            .as_ref()
            .is_some_and(|form| form.invoice == invoice.id);
        if !has_draft
            && !invoice.has_credit_history
            && (invoice.credit_total >= invoice.total || invoice.reversal.is_some())
        {
            return;
        }
        egui::CollapsingHeader::new(format!("Partial {label}s"))
            .id_salt(("invoice-credits", invoice.id))
            .show(ui, |ui| {
                let notes = match self.store.invoice_credit_notes(company, invoice.id) {
                    Ok(notes) => notes,
                    Err(e) => {
                        self.failure(e);
                        return;
                    }
                };
                for note in notes {
                    ui.push_id(note.id, |ui| {
                        ui.label(format!(
                            "{} · {} · {} · Voucher {} · {}",
                            note.date,
                            note.reference,
                            money(i128::from(note.amount)),
                            note.voucher_id,
                            note.reason
                        ));
                        match self.store.invoice_return_lines(company, note.id) {
                            Ok(lines) => {
                                for line in lines {
                                    ui.label(format!(
                                        "{} {} returned · {} · {} · {}",
                                        format_quantity(line.quantity.into()),
                                        line.unit,
                                        line.item,
                                        line.location,
                                        line.batch.as_deref().unwrap_or("Unbatched")
                                    ));
                                }
                            }
                            Err(e) => self.failure(e),
                        }
                        if ui.button("Export printable note").clicked() {
                            match self.store.export_invoice_credit_html(
                                company,
                                note.id,
                                self.invoice_document_path.trim(),
                            ) {
                                Ok(()) => self.message = "Note document created".into(),
                                Err(e) => self.failure(e),
                            }
                        }
                        let has_refunds = match self.invoice_refund_panel(ui, company, invoice, &note) {
                            Ok(active) => active,
                            Err(e) => { self.failure(e); return; }
                        };
                        if let Some(voucher) = note.reversal_voucher_id {
                            ui.label(format!(
                                "Reversed {} · Voucher {} · {}",
                                note.reversal_date.as_deref().unwrap_or_default(),
                                voucher,
                                note.reversal_reason.as_deref().unwrap_or_default(),
                            ));
                        } else if ui
                            .add_enabled(
                                self.invoice_credit.is_none() && self.invoice_refund.is_none() && !has_refunds,
                                egui::Button::new("Reverse note"),
                            )
                            .on_disabled_hover_text("Reverse refunds and release applied credit before reversing the note.")
                            .clicked()
                        {
                            self.invoice_credit =
                                Some(NoteForm::new(invoice.id, Some(note.id), Vec::new()));
                        }
                    });
                }
                let can_add = self.invoice_credit.is_none()
                    && invoice.credit_total < invoice.total
                    && invoice.reversal.is_none()
                    && self.editing_invoice != Some(invoice.id);
                if ui
                    .add_enabled(can_add, egui::Button::new(format!("Add partial {label}")))
                    .clicked()
                {
                    self.start_invoice_note(company, invoice.id, false);
                }
                if !invoice.lines.is_empty()
                    && ui
                        .add_enabled(can_add, egui::Button::new("Return invoiced items"))
                        .clicked()
                {
                    self.start_invoice_note(company, invoice.id, true);
                }
                self.edit_invoice_note(ui, company, invoice);
            });
    }

    fn start_invoice_note(&mut self, company: i64, invoice: i64, stock: bool) {
        let result = (|| -> Result<NoteForm> {
            let mut components = self.store.invoice_credit_components(company, invoice)?;
            if stock {
                components.retain(|part| part.is_tax);
            }
            let mut form = NoteForm::new(invoice, None, components);
            if stock {
                form.returns = self
                    .store
                    .invoice_return_allocations(company, invoice)?
                    .into_iter()
                    .map(|part| (part, String::new()))
                    .collect();
                if form.returns.is_empty() {
                    return Err(Error::Validation(
                        "This invoice has no quantities available to return".into(),
                    ));
                }
            }
            Ok(form)
        })();
        match result {
            Ok(form) => self.invoice_credit = Some(form),
            Err(e) => self.failure(e),
        }
    }

    fn edit_invoice_note(&mut self, ui: &mut egui::Ui, company: i64, invoice: &InvoiceSummary) {
        let Some(form) = self
            .invoice_credit
            .as_mut()
            .filter(|form| form.invoice == invoice.id)
        else {
            return;
        };
        if form.reversal.is_none() {
            if !form.returns.is_empty() {
                egui::Grid::new(("invoice-return-items", invoice.id)).show(ui, |ui| {
                    for title in ["Item", "Location", "Batch", "Returnable", "Return quantity"] {
                        ui.strong(title);
                    }
                    ui.end_row();
                    for (part, quantity) in &mut form.returns {
                        ui.label(&part.item);
                        ui.label(&part.location);
                        ui.label(part.batch.as_deref().unwrap_or("Unbatched"));
                        ui.label(format!(
                            "{} {}",
                            format_quantity(part.remaining.into()),
                            part.unit
                        ));
                        ui.add(egui::TextEdit::singleline(quantity).desired_width(80.0));
                        ui.end_row();
                    }
                });
                if let Ok(quantities) = form.quantities() {
                    if !quantities.is_empty() {
                        match self
                            .store
                            .invoice_return_subtotal(company, invoice.id, &quantities)
                        {
                            Ok(subtotal) => {
                                ui.label(format!("Return subtotal before tax {}", money(subtotal)));
                            }
                            Err(e) => {
                                ui.label(e.to_string());
                            }
                        }
                    }
                }
            }
            ui.small(format!(
                "Unpaid balance {}. Any excess can be refunded or applied to another bill.",
                money(invoice.outstanding)
            ));
            if !form.lines.is_empty() {
                egui::Grid::new(("note-components", invoice.id)).show(ui, |ui| {
                    for heading in ["Component", "Remaining", "Amount"] {
                        ui.strong(heading);
                    }
                    ui.end_row();
                    for (part, value) in &mut form.lines {
                        let category = if part.is_tax {
                            " (tax)"
                        } else if part.remaining < 0 {
                            " (deduction)"
                        } else {
                            ""
                        };
                        ui.label(format!("{}{category}", part.ledger));
                        ui.label(money(part.remaining));
                        ui.add(egui::TextEdit::singleline(value).desired_width(90.0));
                        ui.end_row();
                    }
                });
            }
        }
        egui::Grid::new(("note-fields", invoice.id)).show(ui, |ui| {
            for (label, value) in [
                ("Date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let button = if form.reversal.is_some() {
            "Post reversal"
        } else {
            "Post note"
        };
        let (save, cancel) = ui
            .horizontal(|ui| (ui.button(button).clicked(), ui.button("Cancel").clicked()))
            .inner;
        if cancel {
            self.invoice_credit = None;
        } else if save {
            let result = if let Some(note) = form.reversal {
                self.store
                    .reverse_invoice_credit(
                        company,
                        note,
                        &form.date,
                        &form.reference,
                        &form.reason,
                    )
                    .map(|id| format!("Note reversal voucher {id} posted"))
            } else if !form.returns.is_empty() {
                (|| {
                    self.store.post_invoice_stock_return(
                        company,
                        invoice.id,
                        &form.date,
                        &form.reference,
                        &form.reason,
                        &form.quantities()?,
                        &form.amounts()?,
                    )
                })()
                .map(|id| format!("Invoice stock return {id} posted"))
            } else {
                form.amounts()
                    .and_then(|lines| {
                        self.store.post_invoice_credit(
                            company,
                            invoice.id,
                            &form.date,
                            &form.reference,
                            &form.reason,
                            &lines,
                        )
                    })
                    .map(|id| format!("Invoice note {id} posted"))
            };
            match result {
                Ok(message) => {
                    self.message = message;
                    self.invoice_credit = None;
                }
                Err(e) => self.failure(e),
            }
        }
    }
}
