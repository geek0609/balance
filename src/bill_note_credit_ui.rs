use super::{money, outstanding_bill_choices, pick_settlement_bill, App, BillSettlementTarget};
use balance::{parse_amount, Error, Result};
use eframe::egui;

pub(super) enum NoteCreditAction {
    Apply {
        credit: i64,
        party: i64,
        kind: balance::inventory::InvoiceKind,
        bill: Option<BillSettlementTarget>,
        date: String,
        amount: String,
    },
    Release {
        application: i64,
        date: String,
        reason: String,
    },
}

impl App {
    pub(super) fn bill_note_credits_panel(&mut self, ui: &mut egui::Ui, company: i64) {
        let through = self.outstanding_date.trim();
        let credits = match self.store.bill_note_credits(company, Some(through)) {
            Ok(credits) => credits,
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                return;
            }
        };
        if credits.is_empty() && self.note_credit_action.is_none() {
            return;
        }
        crate::theme::section(ui, "Named note credits");
        let mut action = None;
        for credit in credits {
            egui::CollapsingHeader::new(format!(
                "{} · {} · {} remaining",
                credit.party,
                credit.reference,
                money(credit.available)
            ))
            .id_salt(("note-credit", credit.id))
            .show(ui, |ui| {
                ui.label(format!(
                    "{} · Voucher {} (#{}), originally {}",
                    credit.date,
                    credit.voucher_reference,
                    credit.voucher_id,
                    money(i128::from(credit.amount))
                ));
                if let Some(due) = &credit.due_date {
                    ui.small(format!("Due {due}"));
                }
                if let Some(days) = credit.credit_days {
                    ui.small(format!("Credit period {days} days"));
                }
                if let Some(reversed) = &credit.reversal_date {
                    ui.small(format!("Reversed on {reversed}"));
                } else if credit.available > 0 && ui.button("Apply to a bill").clicked() {
                    action = Some(NoteCreditAction::Apply {
                        credit: credit.id,
                        party: credit.party_ledger_id,
                        kind: credit.kind,
                        bill: None,
                        date: through.into(),
                        amount: String::new(),
                    });
                }
                match self.store.bill_note_credit_applications(company, credit.id) {
                    Ok(rows) => {
                        for row in rows.into_iter().filter(|row| row.date.as_str() <= through) {
                            ui.horizontal(|ui| {
                                ui.small(format!(
                                    "{} · Bill {} · {}",
                                    row.date,
                                    row.bill_reference,
                                    money(i128::from(row.amount))
                                ));
                                if let Some(date) = &row.released_on {
                                    let reason = row.release_reason.as_deref().unwrap_or_default();
                                    ui.small(format!("Released on {date} · {reason}"));
                                } else if ui.button("Release").clicked() {
                                    action = Some(NoteCreditAction::Release {
                                        application: row.id,
                                        date: through.into(),
                                        reason: String::new(),
                                    });
                                }
                            });
                        }
                    }
                    Err(error) => {
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                    }
                }
            });
        }
        if action.is_some() {
            self.note_credit_action = action;
        }
        let Some(form) = &mut self.note_credit_action else {
            return;
        };
        let mut result = None::<Result<&str>>;
        match form {
            NoteCreditAction::Apply {
                credit,
                party,
                kind,
                bill,
                date,
                amount,
            } => {
                match self.store.invoices(company) {
                    Ok(invoices) => {
                        let choices = outstanding_bill_choices(invoices.iter().filter(|invoice| {
                            invoice.party_ledger_id == *party && invoice.kind == *kind
                        }));
                        pick_settlement_bill(ui, "named-credit-bill", &choices, bill);
                    }
                    Err(error) => {
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("Application date");
                    ui.text_edit_singleline(date);
                    ui.label("Amount");
                    ui.text_edit_singleline(amount);
                    if ui.button("Apply credit").clicked() {
                        result = Some((|| {
                            let target =
                                bill.ok_or_else(|| Error::Validation("Select a bill".into()))?;
                            self.store.apply_bill_note_credit(
                                company,
                                *credit,
                                target,
                                date,
                                parse_amount(amount)?,
                            )?;
                            Ok("Credit applied")
                        })());
                    }
                });
            }
            NoteCreditAction::Release {
                application,
                date,
                reason,
            } => {
                ui.horizontal(|ui| {
                    ui.label("Release date");
                    ui.text_edit_singleline(date);
                    ui.label("Reason");
                    ui.text_edit_singleline(reason);
                    if ui.button("Release credit").clicked() {
                        result = Some(
                            self.store
                                .release_bill_note_credit(company, *application, date, reason)
                                .map(|()| "Credit released"),
                        );
                    }
                });
            }
        }
        if ui.button("Cancel").clicked() {
            self.note_credit_action = None;
        }
        if let Some(result) = result {
            match result {
                Ok(message) => {
                    self.note_credit_action = None;
                    self.message = message.into();
                }
                Err(error) => self.failure(error),
            }
        }
    }
}
