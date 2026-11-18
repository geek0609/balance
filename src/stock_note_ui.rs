use super::{
    pick_account, pick_batch, pick_location, pick_stock, App, InvoiceLineForm, InvoiceSource, Page,
};
use balance::inventory::{format_quantity, parse_quantity, InvoiceKind};
use balance::orders::OrderDetail;
use balance::stock_notes::{
    note_label, StockNote, StockNoteDraft, StockNoteLine, StockNoteLineDraft,
};
use balance::{format_amount, parse_amount, Category, Error, Result};
use eframe::egui;

#[derive(Default)]
struct NoteLineForm {
    item: Option<i64>,
    location: Option<i64>,
    batch: Option<i64>,
    quantity: String,
    price: String,
}

struct NoteForm {
    kind: InvoiceKind,
    order: Option<i64>,
    party: Option<i64>,
    reference: String,
    date: String,
    narration: String,
    lines: Vec<NoteLineForm>,
}

impl Default for NoteForm {
    fn default() -> Self {
        Self {
            kind: InvoiceKind::Sale,
            order: None,
            party: None,
            reference: String::new(),
            date: String::new(),
            narration: String::new(),
            lines: vec![NoteLineForm::default()],
        }
    }
}

impl NoteForm {
    fn draft(&self) -> Result<StockNoteDraft> {
        let lines = self
            .lines
            .iter()
            .map(|line| {
                Ok(StockNoteLineDraft {
                    item_id: line
                        .item
                        .ok_or_else(|| Error::Validation("Select an item for each line".into()))?,
                    location_id: line.location.ok_or_else(|| {
                        Error::Validation("Select a location for each line".into())
                    })?,
                    batch_id: line.batch,
                    quantity: parse_quantity(&line.quantity)?,
                    unit_price: parse_amount(&line.price)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(StockNoteDraft {
            kind: self.kind,
            date: self.date.clone(),
            reference: self.reference.clone(),
            party_ledger_id: self
                .party
                .ok_or_else(|| Error::Validation("Select a party ledger".into()))?,
            order_id: self.order,
            narration: self.narration.clone(),
            lines,
        })
    }
}

#[derive(Default)]
pub(super) struct StockNoteScreen {
    form: NoteForm,
    selected: Option<i64>,
    reverse_date: String,
    reverse_reason: String,
    document_path: String,
}

impl StockNoteScreen {
    fn select(&mut self, id: Option<i64>) {
        self.selected = id;
        self.reverse_date.clear();
        self.reverse_reason.clear();
    }
}

impl App {
    pub(super) fn open_stock_note(&mut self, note: i64) {
        self.stock_note_screen.select(Some(note));
        self.page = Page::StockNotes;
        self.message.clear();
    }

    pub(super) fn prepare_order_stock_note(&mut self, detail: &OrderDetail) {
        let order = &detail.summary;
        self.stock_note_screen.form = NoteForm {
            kind: order.kind,
            order: Some(order.id),
            party: Some(order.party_ledger_id),
            lines: detail
                .lines
                .iter()
                .filter(|line| line.unfulfilled() > 0)
                .map(|line| NoteLineForm {
                    item: Some(line.line.item_id),
                    quantity: format_quantity(i128::from(line.unfulfilled())),
                    price: format_amount(line.line.unit_price),
                    ..NoteLineForm::default()
                })
                .collect(),
            ..NoteForm::default()
        };
        self.page = Page::StockNotes;
        self.message.clear();
    }

    pub(super) fn stock_notes_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Delivery and receipt notes");
        if let Err(error) = self.stock_note_form(ui, company) {
            self.failure(error);
        }
        ui.separator();
        if let Err(error) = self.stock_note_register(ui, company) {
            self.failure(error);
        }
    }

    fn stock_note_form(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        let items = self.store.stock_items(company)?;
        let locations = self.store.stock_locations(company)?;
        let default_location = locations
            .iter()
            .find(|location| location.name == "Main")
            .map(|location| location.id);
        let ledgers = self
            .store
            .ledgers(company)?
            .into_iter()
            .filter(|ledger| matches!(ledger.category, Category::Asset | Category::Liability))
            .collect::<Vec<_>>();
        ui.heading("New stock note");
        let form = &mut self.stock_note_screen.form;
        if let Some(order) = form.order {
            ui.horizontal(|ui| {
                ui.label(format!("Order {order}"));
                if ui.button("Clear order draft").clicked() {
                    *form = NoteForm::default();
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(form.order.is_none(), |ui| {
                egui::ComboBox::from_id_salt("stock-note-type")
                    .selected_text(note_label(form.kind))
                    .show_ui(ui, |ui| {
                        for kind in InvoiceKind::ALL {
                            ui.selectable_value(&mut form.kind, kind, note_label(kind));
                        }
                    });
                pick_account(
                    ui,
                    "stock-note-party",
                    "Party",
                    &ledgers,
                    &mut form.party,
                    false,
                );
            });
            ui.label("Date");
            ui.add(egui::TextEdit::singleline(&mut form.date).hint_text("YYYY-MM-DD"));
            ui.label("Reference");
            ui.text_edit_singleline(&mut form.reference);
        });
        ui.horizontal(|ui| {
            ui.label("Narration");
            ui.text_edit_singleline(&mut form.narration);
        });
        if form.kind == InvoiceKind::Purchase {
            ui.small("The receipt price records the acquisition cost on the receipt date.");
        }
        let mut remove = None;
        for (index, line) in form.lines.iter_mut().enumerate() {
            if line.location.is_none() {
                line.location = default_location;
            }
            ui.horizontal_wrapped(|ui| -> Result<()> {
                let previous_item = line.item;
                pick_stock(
                    ui,
                    ("stock-note-item", index),
                    "Item",
                    &items,
                    &mut line.item,
                );
                if previous_item != line.item {
                    line.batch = None;
                }
                pick_location(
                    ui,
                    ("stock-note-location", index),
                    "Location",
                    &locations,
                    &mut line.location,
                    false,
                );
                if let Some(item) = line.item {
                    let batches = self.store.stock_batches(company, item)?;
                    pick_batch(ui, ("stock-note-batch", index), &batches, &mut line.batch);
                }
                ui.add(
                    egui::TextEdit::singleline(&mut line.quantity)
                        .hint_text("Quantity")
                        .desired_width(90.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut line.price)
                        .hint_text("Unit price")
                        .desired_width(90.0),
                );
                if ui.button("Remove line").clicked() {
                    remove = Some(index);
                }
                Ok(())
            })
            .inner?;
        }
        if let Some(index) = remove {
            form.lines.remove(index);
        }
        if ui.button("Add item line").clicked() {
            form.lines.push(NoteLineForm::default());
        }
        if ui
            .button(format!("Post {}", note_label(form.kind).to_lowercase()))
            .clicked()
        {
            let id = self.store.post_stock_note(company, &form.draft()?)?;
            self.stock_note_screen.form = NoteForm::default();
            self.stock_note_screen.select(Some(id));
            self.message = format!("Stock note {id} posted");
        }
        Ok(())
    }

    fn stock_note_register(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        ui.heading("Stock note register");
        let notes = self.store.stock_notes(company, None)?;
        if notes.is_empty() {
            ui.label("No delivery or receipt notes posted.");
            return Ok(());
        }
        let mut selected = self.stock_note_screen.selected;
        let current = notes.iter().find(|note| Some(note.id) == selected);
        let option = |note: &StockNote| {
            format!(
                "{} · {} {} · {} · {}",
                note.date,
                note_label(note.kind),
                note.reference,
                note.party,
                note.status()
            )
        };
        egui::ComboBox::from_id_salt("stock-note-register")
            .selected_text(
                current
                    .map(option)
                    .unwrap_or_else(|| "Select a stock note".into()),
            )
            .show_ui(ui, |ui| {
                for note in &notes {
                    ui.selectable_value(&mut selected, Some(note.id), option(note));
                }
            });
        if selected != self.stock_note_screen.selected {
            self.stock_note_screen.select(selected);
        }
        let Some(note) = notes.iter().find(|note| Some(note.id) == selected) else {
            return Ok(());
        };

        if let Some(order) = note.order_id {
            if ui.link(format!("View linked order {order}")).clicked() {
                self.order_screen.select(order);
                self.page = Page::Orders;
                self.message.clear();
            }
        }
        if !note.narration.is_empty() {
            ui.label(&note.narration);
        }

        let lines = self.store.stock_note_lines(company, note.id)?;
        egui::Grid::new(("stock-note-lines", note.id))
            .striped(true)
            .show(ui, |ui| {
                for heading in ["Item", "Location", "Batch", "Quantity", "Unit price"] {
                    ui.strong(heading);
                }
                ui.end_row();
                for line in &lines {
                    ui.label(&line.item);
                    ui.label(&line.location);
                    ui.label(line.batch.as_deref().unwrap_or("Unbatched"));
                    ui.label(format!(
                        "{} {}",
                        format_quantity(i128::from(line.values.quantity)),
                        line.unit
                    ));
                    ui.label(format_amount(line.values.unit_price));
                    ui.end_row();
                }
            });

        for invoice in self.store.stock_note_invoices(company, note.id)? {
            let mut label = format!(
                "Invoice {} · {} · {}",
                invoice.invoice_id, invoice.date, invoice.reference
            );
            if let Some(reversed) = invoice.reversed_on {
                label.push_str(&format!(" · reversed {reversed}"));
            }
            ui.label(label);
        }

        ui.horizontal(|ui| {
            ui.label("Document path");
            ui.add(
                egui::TextEdit::singleline(&mut self.stock_note_screen.document_path)
                    .hint_text("Absolute path to a new .html file"),
            );
            if ui.button("Export printable note").clicked() {
                match self.store.export_stock_note_html(
                    company,
                    note.id,
                    self.stock_note_screen.document_path.trim(),
                ) {
                    Ok(()) => self.message = "Stock note document exported".into(),
                    Err(error) => self.failure(error),
                }
            }
        });
        if let Some(date) = &note.reversed_on {
            ui.label(format!(
                "Stock reversed {date} · {}",
                note.reversal_reason.as_deref().unwrap_or_default()
            ));
        } else if note.active_invoice_id.is_none() {
            if ui
                .add_enabled(
                    !self.invoice_draft_in_use(),
                    egui::Button::new("Prepare invoice"),
                )
                .clicked()
            {
                self.prepare_stock_note_invoice(note, &lines);
            }
            ui.collapsing("Reverse stock movement", |ui| -> Result<()> {
                let form = &mut self.stock_note_screen;
                ui.horizontal(|ui| {
                    ui.label("Reversal date");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.reverse_date).hint_text("YYYY-MM-DD"),
                    );
                    ui.label("Reason");
                    ui.text_edit_singleline(&mut form.reverse_reason);
                });
                if ui.button("Post stock reversal").clicked() {
                    self.store.reverse_stock_note(
                        company,
                        note.id,
                        &form.reverse_date,
                        &form.reverse_reason,
                    )?;
                    form.select(Some(note.id));
                    self.message = format!("Stock note {} reversed", note.id);
                }
                Ok(())
            })
            .body_returned
            .transpose()?;
        }
        Ok(())
    }

    fn prepare_stock_note_invoice(&mut self, note: &StockNote, lines: &[StockNoteLine]) {
        let lines = lines
            .iter()
            .map(|line| {
                let mut form = InvoiceLineForm::new();
                form.item = Some(line.values.item_id);
                form.location = Some(line.values.location_id);
                form.batch = line.values.batch_id;
                form.quantity = format_quantity(i128::from(line.values.quantity));
                form.price = format_amount(line.values.unit_price);
                form
            })
            .collect();
        self.prepare_linked_invoice(
            InvoiceSource::StockNote(note.id),
            note.kind,
            note.party_ledger_id,
            lines,
        );
    }
}
