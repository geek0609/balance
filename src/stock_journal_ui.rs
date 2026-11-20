use crate::{egui, App};
use balance::format_amount;
use balance::inventory::format_quantity;
use balance::stock_journals::StockJournalEntry;

impl App {
    pub(super) fn stock_journals_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Stock journals");
        ui.add(
            egui::TextEdit::singleline(&mut self.stock_journal_filter)
                .hint_text("Filter by reference, type or date"),
        );
        let journals = match self.store.stock_journals(company) {
            Ok(journals) => journals,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let filter = self.stock_journal_filter.trim().to_lowercase();
        let mut found = false;
        for journal in journals.into_iter().filter(|journal| {
            journal.date.contains(&filter)
                || journal.reference.to_lowercase().contains(&filter)
                || journal.voucher_type.to_lowercase().contains(&filter)
        }) {
            found = true;
            egui::CollapsingHeader::new(format!(
                "{} · {} · {}",
                journal.date, journal.voucher_type, journal.reference
            ))
            .id_salt(("stock-journal", company, journal.id))
            .show(ui, |ui| {
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
            });
        }
        if !found {
            ui.label("No matching stock journals.");
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
