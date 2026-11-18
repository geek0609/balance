use crate::{App, Page};
use balance::{format_amount, Result, Store};
use chrono::NaiveDate;
use eframe::egui;

#[derive(Default)]
pub(crate) struct RecurringJournalScreen {
    selected: Option<i64>,
    reviewed_date: Option<NaiveDate>,
    skip_reason: String,
    close_reason: String,
}

impl RecurringJournalScreen {
    fn select(&mut self, id: Option<i64>) {
        *self = Self {
            selected: id,
            ..Self::default()
        };
    }

    fn show(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        company: i64,
    ) -> Result<Option<String>> {
        ui.heading("Monthly journals");
        let journals = store.monthly_journals(company)?;
        if journals.is_empty() {
            ui.label("Save a journal as a monthly schedule from the Voucher page.");
            return Ok(None);
        }

        let selected_label = journals
            .iter()
            .find(|journal| self.selected == Some(journal.id))
            .map(|journal| journal.name.as_str())
            .unwrap_or("Select a schedule");
        let mut selected = self.selected;
        egui::ComboBox::from_label("Schedule")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for journal in &journals {
                    let label = if journal.closed_at.is_some() {
                        format!("{} (closed, #{})", journal.name, journal.id)
                    } else {
                        journal.name.clone()
                    };
                    ui.selectable_value(&mut selected, Some(journal.id), label);
                }
            });
        if selected != self.selected {
            self.select(selected);
        }
        let Some(journal) = journals.iter().find(|journal| selected == Some(journal.id)) else {
            return Ok(None);
        };

        ui.label(format!("Starts {}", journal.first_date));
        if !journal.narration.is_empty() {
            ui.label(&journal.narration);
        }
        let ledgers = store.ledgers(company)?;
        let postings = store.monthly_journal_lines(company, journal.id)?;
        egui::Grid::new("monthly-journal-postings").show(ui, |ui| {
            ui.strong("Ledger");
            ui.strong("Debit");
            ui.strong("Credit");
            ui.end_row();
            for posting in postings {
                let ledger = ledgers.iter().find(|ledger| ledger.id == posting.ledger_id);
                ui.label(
                    ledger
                        .map(|ledger| ledger.name.as_str())
                        .unwrap_or("Unavailable ledger"),
                );
                let amount = format_amount(posting.amount);
                if posting.amount > 0 {
                    ui.label(&amount);
                    ui.label("");
                } else {
                    ui.label("");
                    ui.label(amount.trim_start_matches('-'));
                }
                ui.end_row();
            }
        });

        if let Some(date) = journal.next_date() {
            ui.separator();
            ui.label(format!("Next scheduled date {date}"));
            if self.reviewed_date != Some(date) {
                if ui.button("Review next occurrence").clicked() {
                    self.reviewed_date = Some(date);
                }
            } else {
                if ui.button(format!("Post journal for {date}")).clicked() {
                    let id = store.post_monthly_journal(company, journal.id, date)?;
                    self.reviewed_date = None;
                    self.skip_reason.clear();
                    return Ok(Some(format!("Journal {id} posted for {date}")));
                }
                ui.horizontal(|ui| {
                    ui.label("Skip reason");
                    ui.text_edit_singleline(&mut self.skip_reason);
                });
                if ui.button(format!("Skip {date}")).clicked() {
                    store.skip_monthly_journal(company, journal.id, date, &self.skip_reason)?;
                    self.reviewed_date = None;
                    self.skip_reason.clear();
                    return Ok(Some(format!("Skipped the {date} occurrence")));
                }
            }
        }

        if let Some(closed) = &journal.closed_at {
            ui.label(format!("Closed {closed} UTC"));
            if let Some(reason) = &journal.close_reason {
                ui.label(reason);
            }
        } else {
            let close = ui
                .collapsing("Close schedule", |ui| {
                    ui.label("Close reason");
                    ui.text_edit_singleline(&mut self.close_reason);
                    ui.button("Close schedule").clicked()
                })
                .body_returned
                .unwrap_or(false);
            if close {
                store.close_monthly_journal(company, journal.id, &self.close_reason)?;
                self.select(Some(journal.id));
                return Ok(Some("Monthly schedule closed".into()));
            }
        }

        ui.separator();
        ui.heading("Occurrence history");
        let history = store.monthly_journal_history(company, journal.id)?;
        if history.is_empty() {
            ui.label("No occurrences recorded.");
        }
        for entry in history {
            let action = match (entry.voucher_id, entry.reversal_id, entry.skip_reason) {
                (Some(id), Some(reversal), _) => format!("Journal #{id}, reversed by #{reversal}"),
                (Some(id), None, _) => format!("Journal #{id}"),
                (None, _, Some(reason)) => format!("Skipped · {reason}"),
                (None, _, None) => "Recorded".into(),
            };
            ui.label(format!("{} · {}", entry.date, action));
            ui.small(format!("Recorded {} UTC", entry.resolved_at));
        }
        Ok(None)
    }
}

impl App {
    pub(crate) fn show_recurring_journals(&mut self, ui: &mut egui::Ui, company: i64) {
        match self.recurring_journals.show(ui, &mut self.store, company) {
            Ok(Some(message)) => self.message = message,
            Ok(None) => {}
            Err(error) => self.failure(error),
        }
    }

    pub(crate) fn open_recurring_journal(&mut self, journal: i64) {
        self.recurring_journals.select(Some(journal));
        self.page = Page::Recurring;
    }
}
