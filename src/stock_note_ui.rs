use super::{
    money, pick_account, pick_batch, pick_location, pick_stock, App, InvoiceLineForm,
    InvoiceSource, Page,
};
use balance::inventory::{format_quantity, parse_quantity, InvoiceKind};
use balance::orders::OrderDetail;
use balance::stock_notes::{
    note_label, StockNote, StockNoteDraft, StockNoteLine, StockNoteLineDraft,
};
use balance::stock_returns::StockReturnLineDraft;
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

#[derive(Default)]
struct ReturnDraft {
    reversal: Option<i64>,
    date: String,
    reference: String,
    reason: String,
    quantities: std::collections::HashMap<i64, String>,
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
    additional_billing_notes: std::collections::BTreeSet<i64>,
    reverse_date: String,
    reverse_reason: String,
    document_path: String,
    reconcile_as_of: String,
    return_draft: Option<ReturnDraft>,
}

impl StockNoteScreen {
    fn select(&mut self, id: Option<i64>) {
        self.selected = id;
        self.additional_billing_notes.clear();
        self.reverse_date.clear();
        self.reverse_reason.clear();
        self.return_draft = None;
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
                .flat_map(|line| {
                    let item = Some(line.line.item_id);
                    let price = format_amount(line.line.unit_price);
                    let allocations = detail.reserved_allocations(line.line.item_id);
                    if allocations.is_empty() {
                        return vec![NoteLineForm {
                            item,
                            price,
                            quantity: format_quantity(line.unfulfilled().into()),
                            ..NoteLineForm::default()
                        }];
                    }
                    allocations
                        .into_iter()
                        .map(|a| NoteLineForm {
                            item,
                            price: price.clone(),
                            location: Some(a.location_id),
                            batch: a.batch_id,
                            quantity: format_quantity(a.quantity.into()),
                        })
                        .collect()
                })
                .collect(),
            ..NoteForm::default()
        };
        self.page = Page::StockNotes;
        self.message.clear();
    }

    pub(super) fn stock_notes_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Delivery and receipt notes");
        if let Err(error) = self.stock_note_form(ui, company) {
            self.failure(error);
        }
        ui.separator();
        ui.collapsing("Receipt cost reconciliation", |ui| {
            if let Err(error) = self.receipt_reconciliation_view(ui, company) {
                ui.label(error.to_string());
            }
        });
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
        crate::theme::section(ui, "New stock note");
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

    fn receipt_reconciliation_view(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        ui.horizontal(|ui| {
            ui.label("As of (optional)");
            ui.add(
                egui::TextEdit::singleline(&mut self.stock_note_screen.reconcile_as_of)
                    .hint_text("YYYY-MM-DD"),
            );
        });
        ui.small("Variance compares net invoice prices with the current recorded receipt cost for the billed quantity. Amounts exclude tax and charges.");
        self.report_export_path_field(ui);
        let date = self.stock_note_screen.reconcile_as_of.trim();
        let as_of = (!date.is_empty()).then_some(date);
        let rows = self.store.receipt_reconciliation(company, as_of)?;
        if ui.button("Export receipt reconciliation as CSV").clicked() {
            self.store.export_receipt_reconciliation_csv(
                company,
                as_of,
                self.report_export_path.trim(),
            )?;
            self.message = "Receipt reconciliation exported".into();
        }
        if rows.is_empty() {
            ui.label("No receipt lines for the selected date.");
            return Ok(());
        }
        egui::Grid::new("receipt-reconciliation")
            .striped(true)
            .show(ui, |ui| {
                for label in [
                    "Receipt",
                    "Supplier",
                    "Item",
                    "Received",
                    "Invoiced",
                    "Returned",
                    "Unbilled",
                    "Receipt cost",
                    "Billed cost",
                    "Invoice net",
                    "Variance",
                ] {
                    ui.strong(label);
                }
                ui.end_row();
                for row in rows {
                    if ui
                        .link(format!("{} · {}", row.note_date, row.reference))
                        .clicked()
                    {
                        self.open_stock_note(row.note_id);
                    }
                    ui.label(&row.supplier);
                    let item = format!("{} · {}", row.item, row.location);
                    ui.label(match &row.batch {
                        Some(batch) => format!("{item} · {batch}"),
                        None => item,
                    });
                    for quantity in [
                        row.received_quantity,
                        row.invoiced_quantity,
                        row.returned_quantity,
                        row.unbilled_quantity(),
                    ] {
                        ui.label(format!(
                            "{} {}",
                            format_quantity(i128::from(quantity)),
                            row.unit
                        ));
                    }
                    for cost in [
                        row.receipt_cost,
                        row.invoiced_receipt_cost,
                        Some(row.invoice_net),
                        row.variance(),
                    ] {
                        ui.label(cost.map(money).unwrap_or_else(|| "—".into()));
                    }
                    ui.end_row();
                }
            });
        Ok(())
    }

    fn stock_note_register(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        crate::theme::section(ui, "Stock note register");
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
                for heading in [
                    "Item",
                    "Location",
                    "Batch",
                    "Quantity",
                    "Invoiced",
                    "Returned",
                    "Unbilled",
                    "Unit price",
                ] {
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
                    ui.label(format_quantity(i128::from(
                        line.billed - line.returned_after_invoice,
                    )));
                    ui.label(format_quantity(i128::from(
                        line.returned + line.returned_after_invoice,
                    )));
                    if note.reversed_on.is_none() {
                        ui.label(format_quantity(i128::from(line.unbilled())));
                    } else {
                        ui.label("—");
                    }
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
        }
        self.stock_return_panel(ui, company, note, &lines)?;
        if note.reversed_on.is_none()
            && note.has_unbilled_stock
            && ui
                .add_enabled(
                    !self.invoice_draft_in_use(),
                    egui::Button::new("Prepare invoice (replace draft)"),
                )
                .clicked()
        {
            self.prepare_stock_notes_invoice(company, std::iter::once(note))?;
        }
        if note.reversed_on.is_none() && note.has_unbilled_stock {
            let eligible: Vec<_> = notes
                .iter()
                .filter(|other| {
                    other.id != note.id
                        && other.kind == note.kind
                        && other.party_ledger_id == note.party_ledger_id
                        && other.reversed_on.is_none()
                        && other.has_unbilled_stock
                })
                .collect();
            if !eligible.is_empty() {
                ui.collapsing("Combine stock notes on one invoice", |ui| -> Result<()> {
                    for other in &eligible {
                        let mut chosen = self
                            .stock_note_screen
                            .additional_billing_notes
                            .contains(&other.id);
                        if ui
                            .checkbox(&mut chosen, format!("{} · {}", other.date, other.reference))
                            .changed()
                        {
                            if chosen {
                                self.stock_note_screen
                                    .additional_billing_notes
                                    .insert(other.id);
                            } else {
                                self.stock_note_screen
                                    .additional_billing_notes
                                    .remove(&other.id);
                            }
                        }
                    }
                    let sources: Vec<_> = eligible
                        .iter()
                        .copied()
                        .filter(|other| {
                            self.stock_note_screen
                                .additional_billing_notes
                                .contains(&other.id)
                        })
                        .collect();
                    if ui
                        .add_enabled(
                            !self.invoice_draft_in_use() && !sources.is_empty(),
                            egui::Button::new("Prepare combined invoice"),
                        )
                        .clicked()
                    {
                        self.prepare_stock_notes_invoice(
                            company,
                            std::iter::once(note).chain(sources),
                        )?;
                    }
                    Ok(())
                })
                .body_returned
                .transpose()?;
            }
        }
        if note.reversed_on.is_none() && !note.has_active_invoices && !note.has_active_returns {
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

    fn stock_return_panel(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        note: &StockNote,
        lines: &[StockNoteLine],
    ) -> Result<()> {
        let returns = self.store.stock_note_returns(company, note.id)?;
        let can_post = note.reversed_on.is_none() && note.has_unbilled_stock;
        if returns.is_empty() && !can_post {
            return Ok(());
        }
        egui::CollapsingHeader::new("Stock returns")
            .id_salt(note.id)
            .show(ui, |ui| -> Result<()> {
                for recorded in &returns {
                    ui.push_id(recorded.id, |ui| -> Result<()> {
                        ui.strong(format!("{} · {}", recorded.date, recorded.reference));
                        ui.label(&recorded.reason);
                        for line in &recorded.lines {
                            ui.label(format!(
                                "{} · {} · {} · {} {}",
                                line.item,
                                line.location,
                                line.batch.as_deref().unwrap_or("Unbatched"),
                                format_quantity(line.quantity.into()),
                                line.unit
                            ));
                        }
                        if ui.button("Export printable return").clicked() {
                            self.store.export_stock_return_html(
                                company,
                                note.id,
                                recorded.id,
                                self.stock_note_screen.document_path.trim(),
                            )?;
                            self.message = "Return document exported".into();
                        }
                        if let Some(date) = &recorded.reversed_on {
                            ui.label(format!(
                                "Reversed {date} · {}",
                                recorded.reversal_reason.as_deref().unwrap_or_default()
                            ));
                        } else if self.stock_note_screen.return_draft.is_none()
                            && ui.button("Reverse return").clicked()
                        {
                            self.stock_note_screen.return_draft = Some(ReturnDraft {
                                reversal: Some(recorded.id),
                                ..Default::default()
                            });
                        }
                        Ok(())
                    })
                    .inner?;
                }
                if can_post
                    && self.stock_note_screen.return_draft.is_none()
                    && ui.button("Return unbilled stock").clicked()
                {
                    self.stock_note_screen.return_draft = Some(ReturnDraft::default());
                }
                self.edit_stock_return(ui, company, note.id, lines)
            })
            .body_returned
            .transpose()?;
        Ok(())
    }

    fn edit_stock_return(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        note: i64,
        lines: &[StockNoteLine],
    ) -> Result<()> {
        let Some(form) = self.stock_note_screen.return_draft.as_mut() else {
            return Ok(());
        };
        egui::Grid::new(("stock-return", note)).show(ui, |ui| {
            ui.label("Date (YYYY-MM-DD)");
            ui.text_edit_singleline(&mut form.date);
            ui.end_row();
            if form.reversal.is_none() {
                ui.label("Reference");
                ui.text_edit_singleline(&mut form.reference);
                ui.end_row();
            }
            ui.label("Reason");
            ui.text_edit_singleline(&mut form.reason);
            ui.end_row();
        });
        if form.reversal.is_none() {
            ui.small("Enter quantities for the unbilled lines to return. Goods move at the original location and batch.");
            egui::Grid::new(("stock-return-quantities", note)).show(ui, |ui| {
                for label in ["Item", "Location", "Batch", "Unbilled", "Return quantity"] {
                    ui.strong(label);
                }
                ui.end_row();
                for line in lines.iter().filter(|line| line.unbilled() > 0) {
                    ui.label(&line.item);
                    ui.label(&line.location);
                    ui.label(line.batch.as_deref().unwrap_or("Unbatched"));
                    ui.label(format!(
                        "{} {}",
                        format_quantity(line.unbilled().into()),
                        line.unit
                    ));
                    ui.add(
                        egui::TextEdit::singleline(form.quantities.entry(line.id).or_default())
                            .desired_width(75.0),
                    );
                    ui.end_row();
                }
            });
        }
        let label = if form.reversal.is_some() {
            "Post return reversal"
        } else {
            "Post return"
        };
        let (post, cancel) = ui
            .horizontal(|ui| (ui.button(label).clicked(), ui.button("Cancel").clicked()))
            .inner;
        if post {
            let message = if let Some(id) = form.reversal {
                self.store
                    .reverse_stock_return(company, id, &form.date, &form.reason)?;
                format!("Stock return {id} reversed")
            } else {
                let quantities = form
                    .quantities
                    .iter()
                    .filter(|(_, value)| !value.trim().is_empty())
                    .map(|(&note_line, value)| {
                        Ok(StockReturnLineDraft {
                            note_line,
                            quantity: parse_quantity(value)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let id = self.store.post_stock_return(
                    company,
                    note,
                    &form.date,
                    &form.reference,
                    &form.reason,
                    &quantities,
                )?;
                format!("Stock return {id} posted")
            };
            self.message = message;
            self.stock_note_screen.return_draft = None;
        } else if cancel {
            self.stock_note_screen.return_draft = None;
        }
        Ok(())
    }

    fn prepare_stock_notes_invoice<'a>(
        &mut self,
        company: i64,
        notes: impl IntoIterator<Item = &'a StockNote>,
    ) -> Result<()> {
        let mut notes = notes.into_iter();
        let first = notes
            .next()
            .ok_or_else(|| Error::Validation("Select a stock note".into()))?;
        let mut lines = Vec::new();
        for note in std::iter::once(first).chain(notes) {
            for line in self
                .store
                .stock_note_lines(company, note.id)?
                .into_iter()
                .filter(|line| line.unbilled() > 0)
            {
                let mut form = InvoiceLineForm::new();
                form.stock_note_line = Some((line.id, note.reference.clone()));
                form.item = Some(line.values.item_id);
                form.location = Some(line.values.location_id);
                form.batch = line.values.batch_id;
                form.quantity = format_quantity(i128::from(line.unbilled()));
                form.price = format_amount(line.values.unit_price);
                lines.push(form);
            }
        }
        self.prepare_linked_invoice(
            InvoiceSource::StockNotes,
            first.kind,
            first.party_ledger_id,
            lines,
        );
        Ok(())
    }
}
