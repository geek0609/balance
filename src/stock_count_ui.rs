use super::{pick_batch, pick_location, pick_stock, App};
use balance::inventory::{format_quantity, parse_quantity};
use balance::stock_counts::{CountLine, StockCountDraft, StockCountLine, StockCountPlan};
use balance::{Error, Result};
use eframe::egui;
use std::collections::HashMap;

#[derive(Default)]
struct CountLineForm {
    item: Option<i64>,
    batch: Option<i64>,
    quantity: String,
}

pub(super) struct StockCountScreen {
    location: Option<i64>,
    date: String,
    reference: String,
    notes: String,
    lines: Vec<CountLineForm>,
    preview: Option<StockCountPlan>,
    selected: Option<i64>,
    search: String,
}

impl Default for StockCountScreen {
    fn default() -> Self {
        Self {
            location: None,
            date: String::new(),
            reference: String::new(),
            notes: String::new(),
            lines: vec![CountLineForm::default()],
            preview: None,
            selected: None,
            search: String::new(),
        }
    }
}

impl StockCountScreen {
    pub(super) fn preview_pending(&self) -> bool {
        self.preview.is_some()
    }

    fn draft(&self) -> Result<StockCountDraft> {
        let location_id = self
            .location
            .ok_or_else(|| Error::Validation("Select a stock location".into()))?;
        let lines = self
            .lines
            .iter()
            .map(|line| {
                Ok(CountLine {
                    item_id: line.item.ok_or_else(|| {
                        Error::Validation("Select a stock item for every line".into())
                    })?,
                    batch_id: line.batch,
                    counted_quantity: parse_quantity(&line.quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(StockCountDraft {
            location_id,
            date: self.date.clone(),
            reference: self.reference.clone(),
            notes: self.notes.clone(),
            lines,
        })
    }
}

impl App {
    pub(super) fn stock_counts_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Physical stock counts");
        if self.stock_count_screen.preview_pending() {
            self.stock_count_preview(ui, company);
        } else {
            self.stock_count_form(ui, company);
        }
        ui.separator();
        self.stock_count_history(ui, company);
    }

    fn stock_count_form(&mut self, ui: &mut egui::Ui, company: i64) {
        let items = match self.store.stock_items(company) {
            Ok(items) => items,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let locations = match self.store.stock_locations(company) {
            Ok(locations) => locations,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let mut batches = HashMap::new();
        for item in self
            .stock_count_screen
            .lines
            .iter()
            .filter_map(|line| line.item)
        {
            if let std::collections::hash_map::Entry::Vacant(entry) = batches.entry(item) {
                match self.store.stock_batches(company, item) {
                    Ok(values) => {
                        entry.insert(values);
                    }
                    Err(error) => {
                        self.failure(error);
                        return;
                    }
                }
            }
        }
        let form = &mut self.stock_count_screen;
        ui.horizontal(|ui| {
            pick_location(
                ui,
                "count-location",
                "Location",
                &locations,
                &mut form.location,
                false,
            )
        });
        egui::Grid::new("count-fields").show(ui, |ui| {
            for (label, field) in [
                ("Count date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Notes", &mut form.notes),
            ] {
                ui.label(label);
                ui.text_edit_singleline(field);
                ui.end_row();
            }
        });
        ui.small("Enter each counted item and batch. Use zero for an empty bucket. Unbatched counts apply only to stock without a batch.");
        let mut remove = None;
        for (index, line) in form.lines.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let previous = line.item;
                pick_stock(ui, ("count-item", index), "Item", &items, &mut line.item);
                if line.item != previous {
                    line.batch = None;
                }
                if let Some(item_batches) = line.item.and_then(|id| batches.get(&id)) {
                    pick_batch(ui, ("count-batch", index), item_batches, &mut line.batch);
                }
                ui.add(
                    egui::TextEdit::singleline(&mut line.quantity)
                        .hint_text("Counted quantity")
                        .desired_width(105.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            form.lines.remove(index);
        }
        if ui.button("Add item").clicked() {
            form.lines.push(CountLineForm::default());
        }
        if ui.button("Preview count").clicked() {
            match form
                .draft()
                .and_then(|draft| self.store.preview_stock_count(company, &draft))
            {
                Ok(preview) => {
                    self.stock_count_screen.preview = Some(preview);
                    self.message.clear();
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn stock_count_preview(&mut self, ui: &mut egui::Ui, company: i64) {
        let Some(preview) = self.stock_count_screen.preview.as_ref() else {
            return;
        };
        ui.heading(format!(
            "{} | {} | {}",
            preview.reference(),
            preview.location(),
            preview.date()
        ));
        ui.small("Book quantities include all movements already saved through the count date. Posting records the count and adjusts quantities by the difference.");
        count_table(ui, "count-preview", preview.lines(), false);
        let mut edit = false;
        let mut post = false;
        ui.horizontal(|ui| {
            edit = ui.button("Edit count").clicked();
            post = ui.button("Post count").clicked();
        });
        if edit {
            self.stock_count_screen.preview = None;
        } else if post {
            match self.store.post_stock_count(company, preview) {
                Ok(id) => {
                    self.stock_count_screen = StockCountScreen {
                        selected: Some(id),
                        ..StockCountScreen::default()
                    };
                    self.message = format!("Stock count {id} posted");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn stock_count_history(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Count history");
        ui.add(
            egui::TextEdit::singleline(&mut self.stock_count_screen.search)
                .hint_text("Reference or location"),
        );
        let search = self.stock_count_screen.search.trim().to_lowercase();
        let counts = match self.store.stock_counts(company) {
            Ok(counts) => counts,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let mut found = false;
        for count in counts {
            if !search.is_empty()
                && !count.reference.to_lowercase().contains(&search)
                && !count.location.to_lowercase().contains(&search)
                && self.stock_count_screen.selected != Some(count.id)
            {
                continue;
            }
            found = true;
            let selected = self.stock_count_screen.selected == Some(count.id);
            if ui
                .selectable_label(
                    selected,
                    format!("{} | {} | {}", count.date, count.reference, count.location),
                )
                .clicked()
            {
                self.stock_count_screen.selected = if selected { None } else { Some(count.id) };
            }
            if self.stock_count_screen.selected == Some(count.id) {
                ui.small(format!("Posted {} UTC", count.recorded_at));
                if !count.notes.is_empty() {
                    ui.label(&count.notes);
                }
                match self.store.stock_count_lines(company, count.id) {
                    Ok(lines) => count_table(ui, ("count-history", count.id), &lines, true),
                    Err(error) => self.failure(error),
                }
            }
        }
        if !found {
            ui.label("No matching stock counts.");
        }
    }
}

fn count_table(ui: &mut egui::Ui, id: impl std::hash::Hash, lines: &[StockCountLine], saved: bool) {
    egui::Grid::new(id).striped(true).show(ui, |ui| {
        for label in ["Item", "Batch", "Book", "Counted", "Variance"] {
            ui.strong(label);
        }
        if saved {
            ui.strong("Adjustment");
        }
        ui.end_row();
        for line in lines {
            ui.label(&line.item);
            ui.label(line.batch.as_deref().unwrap_or("Unbatched"));
            ui.label(format!(
                "{} {}",
                format_quantity(line.book_quantity.into()),
                line.unit
            ));
            ui.label(format_quantity(line.counted_quantity.into()));
            ui.label(format_quantity(line.variance()));
            if saved {
                ui.label(
                    line.adjustment_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "-".into()),
                );
            }
            ui.end_row();
        }
    });
}
