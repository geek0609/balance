use crate::{egui, pick_batch, pick_location, pick_stock, App};
use balance::inventory::{format_quantity, parse_quantity};
use balance::stock_journals::{
    StockJournal, StockJournalDraft, StockJournalEntry, StockJournalKind, StockJournalLineDraft,
    StockJournalPreview, StockJournalSide,
};
use balance::{format_amount, parse_amount, Error, Result};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct StockJournalScreen {
    kind: StockJournalKind,
    date: String,
    reference: String,
    narration: String,
    lines: Vec<JournalLineForm>,
    preview: Option<StockJournalPreview>,
    filter: String,
    posted: Option<i64>,
    reversal: Option<JournalReversalForm>,
}

struct JournalReversalForm {
    journal_id: i64,
    date: String,
    reference: String,
    reason: String,
}

struct JournalLineForm {
    side: StockJournalSide,
    item: Option<i64>,
    location: Option<i64>,
    batch: Option<i64>,
    quantity: String,
    amount: String,
}

impl JournalLineForm {
    fn new(side: StockJournalSide, location: Option<i64>) -> Self {
        Self {
            side,
            item: None,
            location,
            batch: None,
            quantity: String::new(),
            amount: String::new(),
        }
    }
}

impl StockJournalScreen {
    pub(super) fn has_edits(&self) -> bool {
        !self.lines.is_empty()
            || !self.date.is_empty()
            || !self.reference.is_empty()
            || !self.narration.is_empty()
            || self.kind != StockJournalKind::default()
            || self.reversal.is_some()
    }

    fn draft(&self) -> Result<StockJournalDraft> {
        let lines = self
            .lines
            .iter()
            .map(|line| {
                Ok(StockJournalLineDraft {
                    side: line.side,
                    item_id: line.item.ok_or_else(|| {
                        Error::Validation("Select a stock item for every line".into())
                    })?,
                    location_id: line.location.ok_or_else(|| {
                        Error::Validation("Select a location for every line".into())
                    })?,
                    batch_id: line.batch,
                    quantity: parse_quantity(&line.quantity)?,
                    amount: (!line.amount.trim().is_empty())
                        .then(|| parse_amount(&line.amount))
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(StockJournalDraft {
            kind: self.kind,
            date: self.date.clone(),
            reference: self.reference.clone(),
            narration: self.narration.clone(),
            lines,
        })
    }
}

impl App {
    pub(super) fn stock_journals_page(&mut self, ui: &mut egui::Ui, company: i64) {
        if self.stock_journal_screen.preview.is_some() {
            self.stock_journal_preview(ui, company);
        } else {
            egui::CollapsingHeader::new("New stock journal")
                .id_salt(("new-stock-journal", company))
                .default_open(true)
                .show(ui, |ui| self.stock_journal_form(ui, company));
        }
        ui.separator();
        self.stock_journal_history(ui, company);
    }

    fn stock_journal_form(&mut self, ui: &mut egui::Ui, company: i64) {
        let (items, locations, batches) = match (|| -> Result<_> {
            let items = self.store.stock_items(company)?;
            let locations = self.store.stock_locations(company)?;
            let mut batches = HashMap::new();
            for item in self
                .stock_journal_screen
                .lines
                .iter()
                .filter_map(|line| line.item)
            {
                if let std::collections::hash_map::Entry::Vacant(entry) = batches.entry(item) {
                    entry.insert(self.store.stock_batches(company, item)?);
                }
            }
            Ok((items, locations, batches))
        })() {
            Ok(masters) => masters,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        if items.is_empty() {
            ui.label("Create a stock item before entering a journal.");
            return;
        }
        let form = &mut self.stock_journal_screen;
        egui::Grid::new("stock-journal-fields").show(ui, |ui| {
            ui.label("Type");
            egui::ComboBox::from_id_salt("stock-journal-kind")
                .selected_text(form.kind.label())
                .show_ui(ui, |ui| {
                    for kind in [StockJournalKind::Stock, StockJournalKind::Manufacturing] {
                        ui.selectable_value(&mut form.kind, kind, kind.label());
                    }
                });
            ui.end_row();
            for (label, field) in [
                ("Date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Narration", &mut form.narration),
            ] {
                ui.label(label);
                ui.text_edit_singleline(field);
                ui.end_row();
            }
        });
        let mut remove = None;
        for (index, line) in form.lines.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.strong(line.side.label());
                let previous = line.item;
                pick_stock(ui, ("journal-item", index), "Item", &items, &mut line.item);
                if line.item != previous {
                    line.batch = None;
                }
                pick_location(
                    ui,
                    ("journal-location", index),
                    "Location",
                    &locations,
                    &mut line.location,
                    false,
                );
                if let Some(choices) = line.item.and_then(|item| batches.get(&item)) {
                    pick_batch(ui, ("journal-batch", index), choices, &mut line.batch);
                }
                let quantity_label = items
                    .iter()
                    .find(|item| Some(item.id) == line.item)
                    .map_or_else(|| "Quantity".into(), |item| format!("Qty ({})", item.unit));
                ui.add(
                    egui::TextEdit::singleline(&mut line.quantity)
                        .hint_text(quantity_label)
                        .desired_width(84.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut line.amount)
                        .hint_text("Value (optional)")
                        .desired_width(108.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            form.lines.remove(index);
        }
        let default_location = (locations.len() == 1).then(|| locations[0].id);
        let mut preview = false;
        ui.horizontal_wrapped(|ui| {
            for (label, side) in [
                ("Add source", StockJournalSide::Source),
                ("Add destination", StockJournalSide::Destination),
            ] {
                if ui.button(label).clicked() {
                    form.lines
                        .push(JournalLineForm::new(side, default_location));
                }
            }
            preview = ui
                .add_enabled(!form.lines.is_empty(), egui::Button::new("Preview journal"))
                .clicked();
        });
        if preview {
            match form
                .draft()
                .and_then(|draft| self.store.preview_stock_journal(company, &draft))
            {
                Ok(preview) => {
                    form.preview = Some(preview);
                    self.message.clear();
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn stock_journal_preview(&mut self, ui: &mut egui::Ui, company: i64) {
        let Some(preview) = self.stock_journal_screen.preview.as_ref() else {
            return;
        };
        let draft = preview.draft();
        crate::theme::section(
            ui,
            format!(
                "{} · {} · {}",
                draft.kind.label(),
                draft.reference,
                draft.date
            ),
        );
        if !draft.narration.is_empty() {
            ui.label(&draft.narration);
        }
        egui::Grid::new("stock-journal-preview")
            .striped(true)
            .show(ui, |ui| {
                for heading in [
                    "Item",
                    "Location",
                    "Batch",
                    "Before",
                    "Source",
                    "Destination",
                    "After",
                ] {
                    ui.strong(heading);
                }
                ui.end_row();
                for row in preview.balances() {
                    ui.label(format!("{} · {}", row.item, row.unit));
                    ui.label(&row.location);
                    ui.label(row.batch.as_deref().unwrap_or("Unbatched"));
                    for quantity in [row.before, row.issued, row.received, row.after()] {
                        ui.label(format_quantity(quantity));
                    }
                    ui.end_row();
                }
            });
        if draft.lines.iter().any(|line| line.amount.is_some()) {
            egui::CollapsingHeader::new("Recorded values")
                .default_open(true)
                .show(ui, |ui| {
                    for line in &draft.lines {
                        let Some(amount) = line.amount else {
                            continue;
                        };
                        if let Some(row) = preview.balances().iter().find(|row| {
                            row.item_id == line.item_id
                                && row.location_id == line.location_id
                                && row.batch_id == line.batch_id
                        }) {
                            ui.label(format!(
                                "{} · {} · {} · {} · {} {} · {}",
                                line.side.label(),
                                row.item,
                                row.location,
                                row.batch.as_deref().unwrap_or("Unbatched"),
                                format_quantity(line.quantity.into()),
                                row.unit,
                                format_amount(amount)
                            ));
                        }
                    }
                });
        }
        let mut edit = false;
        let mut post = false;
        ui.horizontal(|ui| {
            edit = ui.button("Edit journal").clicked();
            post = ui.button("Post journal").clicked();
        });
        if edit {
            self.stock_journal_screen.preview = None;
        } else if post {
            match self.store.post_stock_journal(company, preview) {
                Ok(id) => {
                    self.message = format!("Stock journal {} posted", draft.reference);
                    self.stock_journal_screen = StockJournalScreen {
                        posted: Some(id),
                        ..StockJournalScreen::default()
                    };
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn stock_journal_history(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Stock journals");
        ui.add(
            egui::TextEdit::singleline(&mut self.stock_journal_screen.filter)
                .hint_text("Filter by reference, type or date"),
        );
        let journals = match self.store.stock_journals(company) {
            Ok(journals) => journals,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let active_reversal = self
            .stock_journal_screen
            .reversal
            .as_ref()
            .map(|form| form.journal_id);
        if active_reversal.is_some_and(|id| {
            !journals.iter().any(|journal| {
                journal.id == id && journal.reversal.is_none() && journal.reverses.is_none()
            })
        }) {
            self.stock_journal_screen.reversal = None;
        }
        let filter = self.stock_journal_screen.filter.trim().to_lowercase();
        let mut found = false;
        for journal in journals.into_iter().filter(|journal| {
            active_reversal == Some(journal.id)
                || journal.date.contains(&filter)
                || journal.reference.to_lowercase().contains(&filter)
                || journal.voucher_type.to_lowercase().contains(&filter)
        }) {
            found = true;
            egui::CollapsingHeader::new(format!(
                "{} · {} · {}",
                journal.date, journal.voucher_type, journal.reference
            ))
            .id_salt(("stock-journal", company, journal.id))
            .default_open(self.stock_journal_screen.posted == Some(journal.id))
            .show(ui, |ui| {
                if let Some(link) = &journal.reversal {
                    ui.label(format!("Reversed by {} on {}", link.reference, link.date));
                }
                if let Some(link) = &journal.reverses {
                    ui.label(format!(
                        "Reversal of {} dated {}",
                        link.reference, link.date
                    ));
                }
                if !journal.narration.is_empty() {
                    ui.label(&journal.narration);
                }
                match self.store.stock_journal_entries(company, journal.id) {
                    Ok(entries) => {
                        for entry in entries {
                            journal_entry(ui, entry);
                        }
                    }
                    Err(error) => self.failure(error),
                }
                if journal.reversal.is_none() && journal.reverses.is_none() {
                    self.stock_journal_reversal_form(ui, company, &journal);
                }
            });
        }
        if !found {
            ui.label("No matching stock journals.");
        }
    }

    fn stock_journal_reversal_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        journal: &StockJournal,
    ) {
        let another_reversal = self
            .stock_journal_screen
            .reversal
            .as_ref()
            .is_some_and(|form| form.journal_id != journal.id);
        let Some(form) = self
            .stock_journal_screen
            .reversal
            .as_mut()
            .filter(|form| form.journal_id == journal.id)
        else {
            if ui
                .add_enabled(!another_reversal, egui::Button::new("Reverse journal"))
                .clicked()
            {
                self.stock_journal_screen.reversal = Some(JournalReversalForm {
                    journal_id: journal.id,
                    date: journal.date.clone(),
                    reference: format!("{}-REV", journal.reference),
                    reason: String::new(),
                });
            }
            return;
        };
        ui.separator();
        ui.small(
            "Restore source quantities and remove destination quantities on the reversal date.",
        );
        egui::Grid::new(("journal-reversal-fields", journal.id)).show(ui, |ui| {
            for (label, field) in [
                ("Reversal date (YYYY-MM-DD)", &mut form.date),
                ("Reversal reference", &mut form.reference),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(field);
                ui.end_row();
            }
        });
        let mut post = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            post = ui.button("Post reversal").clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if cancel {
            self.stock_journal_screen.reversal = None;
        } else if post {
            match self.store.reverse_stock_journal(
                company,
                journal.id,
                &form.date,
                &form.reference,
                &form.reason,
            ) {
                Ok(id) => {
                    self.message = format!("Stock journal {} reversed", journal.reference);
                    self.stock_journal_screen.reversal = None;
                    self.stock_journal_screen.filter.clear();
                    self.stock_journal_screen.posted = Some(id);
                }
                Err(error) => self.failure(error),
            }
        }
    }
}

fn journal_entry(ui: &mut egui::Ui, entry: StockJournalEntry) {
    let direction = if entry.quantity < 0 {
        "Source"
    } else {
        "Destination"
    };
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!(
            "{direction} · {} · {} {}",
            entry.item,
            format_quantity(i128::from(entry.quantity).abs()),
            entry.unit,
        ));
        if let Some(rate) = &entry.rate {
            ui.label(format!("Rate {rate}"));
        }
        if let Some(amount) = entry.amount {
            ui.label(format!("Value {}", format_amount(amount)));
        }
    });
    egui::Grid::new(("stock-journal-entry", entry.id))
        .striped(true)
        .show(ui, |ui| {
            for heading in ["Location", "Batch", "Quantity", "Rate", "Value"] {
                ui.strong(heading);
            }
            ui.end_row();
            for allocation in entry.allocations {
                ui.label(allocation.location);
                ui.label(allocation.batch.as_deref().unwrap_or("Unbatched"));
                ui.label(format_quantity(i128::from(allocation.quantity).abs()));
                ui.label(allocation.rate.as_deref().unwrap_or("—"));
                ui.label(
                    allocation
                        .amount
                        .map(format_amount)
                        .as_deref()
                        .unwrap_or("—"),
                );
                ui.end_row();
            }
        });
}
