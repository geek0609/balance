use crate::{money, pick_account, App};
use balance::inventory::{InvoiceKind, InvoiceSummary};
use balance::invoice_credits::CreditNote;
use balance::{parse_amount, Category, Error, Result};
use eframe::egui;

#[derive(Clone, Copy, Default)]
enum CreditAction {
    #[default]
    Refund,
    ReverseRefund(i64),
    Apply,
    Release(i64),
}

#[derive(Default)]
pub(crate) struct RefundForm {
    note: i64,
    action: CreditAction,
    bank: Option<i64>,
    invoice: Option<i64>,
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
        let applications = self.store.invoice_credit_allocations(company, note.id)?;
        let applied: i128 = applications
            .iter()
            .filter(|row| row.released_on.is_none())
            .map(|row| i128::from(row.amount))
            .sum();
        let available = if note.reversal_voucher_id.is_none() {
            i128::from(note.refundable) - refunded - applied
        } else {
            0
        };
        ui.label(format!(
            "Original bill reduction {} · Credit issued {} · Applied {} · Refunded {} · Available {}",
            money(i128::from(note.amount - note.refundable)),
            money(note.refundable.into()),
            money(applied),
            money(refunded),
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
                        action: CreditAction::ReverseRefund(refund.id),
                        ..Default::default()
                    });
                }
            });
        }
        for application in applications {
            ui.push_id(("credit-allocation", application.id), |ui| {
                ui.label(format!(
                    "Applied {} to {} on {}",
                    money(application.amount.into()),
                    application.invoice_reference,
                    application.date
                ));
                if let Some(date) = application.released_on {
                    ui.label(format!(
                        "Released {date} · {}",
                        application.release_reason.as_deref().unwrap_or_default()
                    ));
                } else if self.invoice_refund.is_none() && ui.button("Release credit").clicked() {
                    self.invoice_refund = Some(RefundForm {
                        note: note.id,
                        action: CreditAction::Release(application.id),
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
        if available > 0
            && self.invoice_refund.is_none()
            && ui.button("Apply credit to a bill").clicked()
        {
            self.invoice_refund = Some(RefundForm {
                note: note.id,
                action: CreditAction::Apply,
                amount: money(available),
                ..Default::default()
            });
        }
        self.edit_invoice_refund(ui, company, invoice, note.id)?;
        Ok(refunded > 0 || applied > 0)
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
        if matches!(form.action, CreditAction::Refund) {
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
        if matches!(form.action, CreditAction::Apply) {
            let bills: Vec<_> = self
                .store
                .invoices(company)?
                .into_iter()
                .filter(|bill| {
                    bill.kind == invoice.kind
                        && bill.party_ledger_id == invoice.party_ledger_id
                        && bill.outstanding > 0
                })
                .collect();
            let label = |bill: &InvoiceSummary| {
                format!(
                    "{} · {} · Due {}",
                    bill.reference,
                    bill.date,
                    money(bill.outstanding)
                )
            };
            egui::ComboBox::from_id_salt(("credit-target", note))
                .selected_text(
                    bills
                        .iter()
                        .find(|bill| Some(bill.id) == form.invoice)
                        .map(label)
                        .unwrap_or_else(|| "Select an outstanding bill".into()),
                )
                .show_ui(ui, |ui| {
                    for bill in &bills {
                        ui.selectable_value(&mut form.invoice, Some(bill.id), label(bill));
                    }
                });
        }
        egui::Grid::new(("invoice-refund", note)).show(ui, |ui| {
            ui.label("Date (YYYY-MM-DD)");
            ui.text_edit_singleline(&mut form.date);
            ui.end_row();
            if matches!(
                form.action,
                CreditAction::Refund | CreditAction::ReverseRefund(_)
            ) {
                ui.label("Reference");
                ui.text_edit_singleline(&mut form.reference);
                ui.end_row();
            }
            if matches!(
                form.action,
                CreditAction::ReverseRefund(_) | CreditAction::Release(_)
            ) {
                ui.label("Reason");
                ui.text_edit_singleline(&mut form.reason);
            } else {
                ui.label("Amount");
                ui.text_edit_singleline(&mut form.amount);
            }
            ui.end_row();
        });
        let label = match (form.action, invoice.kind) {
            (CreditAction::ReverseRefund(_), _) => "Post refund reversal",
            (CreditAction::Apply, _) => "Apply credit",
            (CreditAction::Release(_), _) => "Release allocation",
            (CreditAction::Refund, InvoiceKind::Sale) => "Record payment to customer",
            (CreditAction::Refund, InvoiceKind::Purchase) => "Record receipt from supplier",
        };
        let (save, cancel) = ui
            .horizontal(|ui| (ui.button(label).clicked(), ui.button("Cancel").clicked()))
            .inner;
        if save {
            self.message = match form.action {
                CreditAction::ReverseRefund(refund) => {
                    let voucher = self.store.reverse_invoice_refund(
                        company,
                        refund,
                        &form.date,
                        &form.reference,
                        &form.reason,
                    )?;
                    format!("Refund reversal voucher {voucher} posted")
                }
                CreditAction::Refund => {
                    let bank = form
                        .bank
                        .ok_or_else(|| Error::Validation("Select a cash or bank ledger".into()))?;
                    let voucher = self.store.post_invoice_refund(
                        company,
                        note,
                        bank,
                        &form.date,
                        &form.reference,
                        parse_amount(&form.amount)?,
                    )?;
                    format!("Refund voucher {voucher} posted")
                }
                CreditAction::Apply => {
                    let bill = form
                        .invoice
                        .ok_or_else(|| Error::Validation("Select an outstanding bill".into()))?;
                    self.store.apply_invoice_credit(
                        company,
                        note,
                        bill,
                        &form.date,
                        parse_amount(&form.amount)?,
                    )?;
                    "Credit allocation saved".into()
                }
                CreditAction::Release(allocation) => {
                    self.store.release_invoice_credit(
                        company,
                        allocation,
                        &form.date,
                        &form.reason,
                    )?;
                    "Credit allocation released".into()
                }
            };
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
