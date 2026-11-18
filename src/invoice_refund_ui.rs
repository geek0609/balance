use crate::{money, pick_account, App};
use balance::inventory::{InvoiceKind, InvoiceSummary};
use balance::invoice_credits::CreditNote;
use balance::{parse_amount, Category, Error, Result};
use eframe::egui;

#[derive(Default)]
pub(crate) struct RefundForm {
    note: i64,
    reversal: Option<i64>,
    bank: Option<i64>,
    date: String,
    reference: String,
    amount: String,
    reason: String,
}

impl App {
    pub(crate) fn invoice_refund_panel(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        invoice: &InvoiceSummary,
        note: &CreditNote,
    ) -> Result<bool> {
        if note.refundable == 0 {
            return Ok(false);
        }
        let history = self.store.invoice_refunds(company, note.id)?;
        let refunded: i128 = history
            .iter()
            .filter(|refund| refund.reversal_date.is_none())
            .map(|refund| i128::from(refund.amount))
            .sum();
        let available = if note.reversal_voucher_id.is_none() {
            i128::from(note.refundable) - refunded
        } else {
            0
        };
        ui.label(format!(
            "Applied to invoice {} · Refundable credit {} · Remaining {}",
            money(i128::from(note.amount - note.refundable)),
            money(note.refundable.into()),
            money(available)
        ));
        for refund in history {
            ui.push_id(refund.id, |ui| {
                ui.label(format!(
                    "Refund {} · {} · {} · {} · Voucher {}",
                    refund.date,
                    refund.reference,
                    refund.bank,
                    money(refund.amount.into()),
                    refund.voucher_id
                ));
                if let Some(date) = refund.reversal_date {
                    ui.label(format!(
                        "Reversed {date} · {} · {}",
                        refund.reversal_reference.as_deref().unwrap_or_default(),
                        refund.reversal_reason.as_deref().unwrap_or_default()
                    ));
                } else if self.invoice_refund.is_none() && ui.button("Reverse refund").clicked() {
                    self.invoice_refund = Some(RefundForm {
                        note: note.id,
                        reversal: Some(refund.id),
                        ..Default::default()
                    });
                }
            });
        }
        if available > 0 && self.invoice_refund.is_none() && ui.button("Record refund").clicked() {
            self.invoice_refund = Some(RefundForm {
                note: note.id,
                amount: money(available),
                ..Default::default()
            });
        }
        self.edit_invoice_refund(ui, company, invoice, note.id)?;
        Ok(refunded > 0)
    }

    fn edit_invoice_refund(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        invoice: &InvoiceSummary,
        note: i64,
    ) -> Result<()> {
        let Some(form) = self
            .invoice_refund
            .as_mut()
            .filter(|form| form.note == note)
        else {
            return Ok(());
        };
        if form.reversal.is_none() {
            let ledgers = self
                .store
                .ledgers(company)?
                .into_iter()
                .filter(|ledger| {
                    ledger.category == Category::Asset && ledger.id != invoice.party_ledger_id
                })
                .collect::<Vec<_>>();
            pick_account(
                ui,
                ("refund-bank", note),
                "Cash or bank",
                &ledgers,
                &mut form.bank,
                false,
            );
        }
        egui::Grid::new(("invoice-refund", note)).show(ui, |ui| {
            for (label, value) in [
                ("Date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
            if form.reversal.is_some() {
                ui.label("Reason");
                ui.text_edit_singleline(&mut form.reason);
            } else {
                ui.label("Amount");
                ui.text_edit_singleline(&mut form.amount);
            }
            ui.end_row();
        });
        let label = match (form.reversal, invoice.kind) {
            (Some(_), _) => "Post refund reversal",
            (None, InvoiceKind::Sale) => "Record payment to customer",
            (None, InvoiceKind::Purchase) => "Record receipt from supplier",
        };
        let (save, cancel) = ui
            .horizontal(|ui| (ui.button(label).clicked(), ui.button("Cancel").clicked()))
            .inner;
        if save {
            let voucher = if let Some(refund) = form.reversal {
                self.store.reverse_invoice_refund(
                    company,
                    refund,
                    &form.date,
                    &form.reference,
                    &form.reason,
                )?
            } else {
                let bank = form
                    .bank
                    .ok_or_else(|| Error::Validation("Select a cash or bank ledger".into()))?;
                self.store.post_invoice_refund(
                    company,
                    note,
                    bank,
                    &form.date,
                    &form.reference,
                    parse_amount(&form.amount)?,
                )?
            };
            self.message = format!("Refund voucher {voucher} posted");
            self.invoice_refund = None;
        } else if cancel {
            self.invoice_refund = None;
        }
        Ok(())
    }

    pub(crate) fn refund_balances_panel(&mut self, ui: &mut egui::Ui, company: i64) {
        egui::CollapsingHeader::new("Refund balances").show(ui, |ui| {
            let date = self.outstanding_date.trim();
            let balances = match self.store.invoice_refund_balances(company, date) {
                Ok(balances) => balances,
                Err(error) => {
                    self.failure(error);
                    return;
                }
            };
            if balances.is_empty() {
                ui.label("No refunds are outstanding on this date.");
                return;
            }
            if ui.button("Export refund balances as CSV").clicked() {
                match self.store.export_refund_balances_csv(
                    company,
                    date,
                    self.report_export_path.trim(),
                ) {
                    Ok(()) => self.message = "Refund balances exported".into(),
                    Err(error) => self.failure(error),
                }
            }
            egui::Grid::new("refund-balances")
                .striped(true)
                .show(ui, |ui| {
                    for label in [
                        "Party",
                        "Invoice",
                        "Note",
                        "Date",
                        "Direction",
                        "Issued",
                        "Remaining",
                    ] {
                        ui.strong(label);
                    }
                    ui.end_row();
                    for row in balances {
                        ui.label(row.party);
                        ui.label(row.invoice_reference);
                        ui.label(row.note_reference);
                        ui.label(row.note_date);
                        ui.label(match row.kind {
                            InvoiceKind::Sale => "Owed to customer",
                            InvoiceKind::Purchase => "Due from supplier",
                        });
                        ui.label(money(row.credit.into()));
                        ui.label(money(row.outstanding.into()));
                        ui.end_row();
                    }
                });
        });
    }
}
