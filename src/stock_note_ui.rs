use super::{
    money, pick_account, pick_batch, pick_location, pick_stock, App, InvoiceLineForm,
    InvoiceSource, Page,
};
use balance::inventory::{format_quantity, parse_quantity, InvoiceKind};
use balance::invoice_stock_links::StockNoteInvoiceAllocation;
use balance::orders::{OrderAllocation, OrderDetail};
use balance::stock_notes::{
    note_label, StockNote, StockNoteDraft, StockNoteInvoice, StockNoteLine, StockNoteLineDraft,
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
    order_id: Option<i64>,
    expected_on: Option<String>,
}

struct NoteForm {
    kind: InvoiceKind,
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

#[derive(Default)]
struct InvoiceMatchForm {
    invoice: Option<i64>,
    date: String,
    quantities: std::collections::HashMap<(i64, i64), String>,
}

#[derive(Default)]
struct StockMatchReleaseForm {
    invoice: Option<i64>,
    date: String,
    reason: String,
}

impl Default for NoteForm {
    fn default() -> Self {
        Self {
            kind: InvoiceKind::Sale,
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
            order_id: self
                .lines
                .first()
                .and_then(|line| line.order_id)
                .filter(|&id| self.lines.iter().all(|line| line.order_id == Some(id))),
            narration: self.narration.clone(),
            lines,
        })
    }

    fn order_sources(&self) -> Vec<Option<OrderAllocation<'_>>> {
        self.lines
            .iter()
            .map(|line| {
                line.order_id.map(|order_id| OrderAllocation {
                    order_id,
                    expected_on: line.expected_on.as_deref(),
                })
            })
            .collect()
    }

    fn add_order(&mut self, detail: &OrderDetail) {
        if let [line] = self.lines.as_slice() {
            if line.item.is_none()
                && line.order_id.is_none()
                && line.quantity.trim().is_empty()
                && line.price.trim().is_empty()
            {
                self.lines.clear();
            }
        }
        self.lines.extend(note_lines_for_order(detail));
    }
}

fn note_lines_for_order(detail: &OrderDetail) -> impl Iterator<Item = NoteLineForm> + '_ {
    detail
        .lines
        .iter()
        .filter(|line| line.unfulfilled() > 0)
        .flat_map(|line| {
            let item = Some(line.line.item_id);
            let order_id = Some(detail.summary.id);
            let price = format_amount(line.line.unit_price);
            let allocations = detail.reserved_allocations(line.line.item_id);
            if allocations.is_empty() {
                return vec![NoteLineForm {
                    item,
                    order_id,
                    price,
                    quantity: format_quantity(line.unfulfilled().into()),
                    ..NoteLineForm::default()
                }];
            }
            allocations
                .into_iter()
                .map(|allocation| NoteLineForm {
                    item,
                    order_id,
                    price: price.clone(),
                    location: Some(allocation.location_id),
                    batch: allocation.batch_id,
                    quantity: format_quantity(allocation.quantity.into()),
                    expected_on: None,
                })
                .collect()
        })
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
    invoice_match: InvoiceMatchForm,
    match_release: StockMatchReleaseForm,
}

impl StockNoteScreen {
    fn select(&mut self, id: Option<i64>) {
        self.selected = id;
        self.additional_billing_notes.clear();
        self.reverse_date.clear();
        self.reverse_reason.clear();
        self.return_draft = None;
        self.invoice_match = InvoiceMatchForm::default();
        self.match_release = StockMatchReleaseForm::default();
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
            party: Some(order.party_ledger_id),
            lines: note_lines_for_order(detail).collect(),
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
        if form.lines.iter().any(|line| line.order_id.is_some())
            && ui.button("Clear order draft").clicked()
        {
            *form = NoteForm::default();
        }
        let has_order = form.lines.iter().any(|line| line.order_id.is_some());
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!has_order, |ui| {
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
        let mut orders =
            crate::order_ui::draft_orders(&self.store, company, form.kind, form.party)?;
        if let Some(order) = crate::order_ui::add_order_items(
            ui,
            &orders,
            form.lines.iter().map(|line| line.order_id),
        ) {
            form.add_order(&self.store.order_detail(company, order)?);
        }
        let order_details = crate::order_ui::draft_order_details(
            &self.store,
            company,
            form.lines.iter().map(|line| line.order_id),
        )?;
        orders.retain(|order| order_details.contains_key(&order.id));
        let mut remove = None;
        for (index, line) in form.lines.iter_mut().enumerate() {
            if line.location.is_none() {
                line.location = default_location;
            }
            ui.horizontal_wrapped(|ui| -> Result<()> {
                let previous_order = line.order_id;
                crate::order_ui::pick_line_order(
                    ui,
                    ("stock-note-order", index),
                    &orders,
                    &mut line.order_id,
                );
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
                if previous_item != line.item || previous_order != line.order_id {
                    line.expected_on = None;
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
                let lots = line
                    .order_id
                    .and_then(|order| order_details.get(&order))
                    .map_or(&[][..], |detail| detail.delivery_lots.as_slice());
                crate::order_ui::pick_delivery_lot(
                    ui,
                    ("stock-note-lot", index),
                    lots,
                    line.item,
                    &mut line.expected_on,
                );
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
            form.lines.push(NoteLineForm {
                order_id: form.lines.iter().find_map(|line| line.order_id),
                ..NoteLineForm::default()
            });
        }
        if ui
            .button(format!("Post {}", note_label(form.kind).to_lowercase()))
            .clicked()
        {
            let id = self.store.post_stock_note_for_orders(
                company,
                &form.draft()?,
                &form.order_sources(),
            )?;
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

        if !note.narration.is_empty() {
            ui.label(&note.narration);
        }

        let lines = self.store.stock_note_lines(company, note.id)?;
        let has_orders = lines.iter().any(|line| line.order_id.is_some());
        let has_trade_defaults = lines.iter().any(|line| line.trade_ledger_id.is_some());
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
                if has_orders {
                    ui.strong("Order");
                }
                if has_trade_defaults {
                    ui.strong("Invoice ledger");
                }
                ui.end_row();
                for line in &lines {
                    let item = ui.label(&line.item);
                    if let Some(tracking) = &line.source_tracking {
                        item.on_hover_text(format!("Tracking {tracking}"));
                    }
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
                    if let (Some(order), Some(reference)) = (line.order_id, &line.order_reference) {
                        if ui.link(reference).clicked() {
                            self.order_screen.select(order);
                            self.page = Page::Orders;
                            self.message.clear();
                        }
                    } else if has_orders {
                        ui.label("—");
                    }
                    if has_trade_defaults {
                        ui.label(line.trade_ledger.as_deref().unwrap_or("—"));
                    }
                    ui.end_row();
                }
            });

        let invoices = self.store.stock_note_invoices(company, note.id)?;
        for invoice in &invoices {
            let mut label = format!(
                "Invoice {} · {} · {}",
                invoice.invoice_id, invoice.date, invoice.reference
            );
            if let Some(reversed) = &invoice.reversed_on {
                label.push_str(&format!(" · reversed {reversed}"));
            }
            if let Some(released) = &invoice.released_on {
                label.push_str(&format!(" · stock match released {released}"));
            }
            ui.label(label);
        }
        if invoices.iter().any(|invoice| invoice.can_release) {
            ui.collapsing("Release a stock match", |ui| {
                self.release_stock_match(ui, company, note, &invoices)
            })
            .body_returned
            .transpose()?;
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
            ui.collapsing("Match stock to an existing invoice", |ui| {
                self.match_prior_invoice(ui, company, note, &lines)
            })
            .body_returned
            .transpose()?;
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

    fn match_prior_invoice(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        note: &StockNote,
        note_lines: &[StockNoteLine],
    ) -> Result<()> {
        let invoices = self
            .store
            .invoices_awaiting_stock_on_note(company, note.id)?;
        if invoices.is_empty() {
            ui.label("No earlier invoice has a matching billed quantity awaiting stock.");
            return Ok(());
        }
        let form = &mut self.stock_note_screen.invoice_match;
        let describe = |date: &str, reference: &str| format!("{date} · {reference}");
        let current = invoices
            .iter()
            .find(|invoice| Some(invoice.id) == form.invoice);
        let mut selected = current.map(|invoice| invoice.id);
        egui::ComboBox::from_id_salt(("match-prior-invoice", note.id))
            .selected_text(current.map_or_else(
                || "Select invoice".to_string(),
                |invoice| describe(&invoice.date, &invoice.reference),
            ))
            .show_ui(ui, |ui| {
                for invoice in &invoices {
                    ui.selectable_value(
                        &mut selected,
                        Some(invoice.id),
                        describe(&invoice.date, &invoice.reference),
                    );
                }
            });
        if form.invoice != selected {
            form.invoice = selected;
            form.quantities.clear();
            form.date = note.date.clone();
        }
        let Some(invoice) = invoices.iter().find(|invoice| Some(invoice.id) == selected) else {
            return Ok(());
        };
        ui.horizontal(|ui| {
            ui.label("Link date");
            ui.add(egui::TextEdit::singleline(&mut form.date).hint_text("YYYY-MM-DD"));
        });
        let mut entries = Vec::new();
        for line in note_lines.iter().filter(|line| line.unbilled() > 0) {
            for allocation in invoice
                .allocations
                .iter()
                .filter(|allocation| invoice.available_on(allocation.allocation_id, line.id) > 0)
            {
                let key = (allocation.allocation_id, line.id);
                let available = invoice.available_on(key.0, key.1);
                let entry = form.quantities.entry(key).or_default();
                ui.horizontal_wrapped(|ui| {
                    if let Some(reference) = &line.order_reference {
                        ui.label(format!("Order {reference}"));
                    }
                    if let Some(due) = &allocation.expected_on {
                        ui.label(format!("Due {due}"));
                    }
                    ui.label(format!(
                        "{} · {} · {} · {} available, {} billed pending (#{}), in {}",
                        line.item,
                        line.location,
                        line.batch.as_deref().unwrap_or("Unbatched"),
                        format_quantity(i128::from(available)),
                        format_quantity(i128::from(allocation.remaining)),
                        allocation.allocation_id,
                        line.unit,
                    ));
                    ui.add(
                        egui::TextEdit::singleline(entry)
                            .hint_text("Qty to match")
                            .desired_width(100.0),
                    );
                });
                if !entry.trim().is_empty() {
                    entries.push((key, entry.clone()));
                }
            }
        }
        if ui.button("Save stock match").clicked() {
            let quantities = entries
                .into_iter()
                .map(|((invoice_allocation_id, note_line_id), qty)| {
                    Ok(StockNoteInvoiceAllocation {
                        invoice_allocation_id,
                        note_line_id,
                        quantity: parse_quantity(&qty)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            self.store.link_invoice_stock_note(
                company,
                note.id,
                invoice.id,
                &form.date,
                &quantities,
            )?;
            *form = InvoiceMatchForm::default();
            self.message = format!("Stock matched to invoice {}", invoice.reference);
        }
        Ok(())
    }

    fn release_stock_match(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        note: &StockNote,
        invoices: &[StockNoteInvoice],
    ) -> Result<()> {
        let form = &mut self.stock_note_screen.match_release;
        let selected = invoices
            .iter()
            .find(|invoice| invoice.can_release && Some(invoice.invoice_id) == form.invoice);
        egui::ComboBox::from_id_salt(("release-stock-match", note.id))
            .selected_text(selected.map_or("Select invoice", |invoice| invoice.reference.as_str()))
            .show_ui(ui, |ui| {
                for invoice in invoices.iter().filter(|invoice| invoice.can_release) {
                    ui.selectable_value(
                        &mut form.invoice,
                        Some(invoice.invoice_id),
                        &invoice.reference,
                    );
                }
            });
        ui.horizontal_wrapped(|ui| {
            ui.label("Release date");
            ui.add(egui::TextEdit::singleline(&mut form.date).hint_text("YYYY-MM-DD"));
            ui.label("Reason");
            ui.text_edit_singleline(&mut form.reason);
        });
        if let Some(invoice) = form.invoice {
            if ui.button("Release matched quantities").clicked() {
                self.store.release_invoice_stock_note(
                    company,
                    note.id,
                    invoice,
                    &form.date,
                    &form.reason,
                )?;
                *form = StockMatchReleaseForm::default();
                self.message = "Matched quantities released".into();
            }
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
                    if let Some(reference) = &line.order_reference {
                        ui.label(format!("{} · Order {reference}", line.item));
                    } else {
                        ui.label(&line.item);
                    }
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
                let source = line.order_reference.as_deref().map_or_else(
                    || note.reference.clone(),
                    |order| format!("{} · Order {order}", note.reference),
                );
                form.stock_note_line = Some((line.id, source));
                form.item = Some(line.values.item_id);
                form.location = Some(line.values.location_id);
                form.batch = line.values.batch_id;
                form.quantity = format_quantity(i128::from(line.unbilled()));
                form.price = format_amount(line.values.unit_price);
                form.trade_ledger = line.trade_ledger_id;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_receipt_combines_purchase_orders_without_merging_their_lines() {
        use balance::orders::{OrderDraft, OrderLine};

        let mut store = balance::Store::memory().unwrap();
        let company = store.create_company("Combined receipt").unwrap();
        let party = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let first = store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Purchase,
                    party_ledger_id: party,
                    date: "2026-04-01".into(),
                    expected_on: None,
                    reference: "PO-A".into(),
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: item,
                        quantity: 1000,
                        unit_price: 1000,
                    }],
                },
            )
            .unwrap();
        let second = store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Purchase,
                    party_ledger_id: party,
                    date: "2026-04-01".into(),
                    expected_on: None,
                    reference: "PO-B".into(),
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: item,
                        quantity: 1000,
                        unit_price: 2300,
                    }],
                },
            )
            .unwrap();
        let first_detail = store.order_detail(company, first).unwrap();
        let second_detail = store.order_detail(company, second).unwrap();
        let mut app = App::new(store, Default::default());
        app.prepare_order_stock_note(&first_detail);
        let form = &mut app.stock_note_screen.form;
        form.add_order(&second_detail);
        form.reference = "RECEIPT".into();
        form.date = "2026-04-02".into();
        for line in &mut form.lines {
            line.location = Some(location);
        }
        let id = app
            .store
            .post_stock_note_for_orders(company, &form.draft().unwrap(), &form.order_sources())
            .unwrap();
        let lines = app.store.stock_note_lines(company, id).unwrap();
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.order_id, line.values.unit_price))
                .collect::<Vec<_>>(),
            [(Some(first), 1000), (Some(second), 2300)]
        );
        assert_eq!(
            app.store
                .stock_receipts(company, None)
                .unwrap()
                .iter()
                .map(|row| row.cost)
                .sum::<Option<i64>>(),
            Some(3300)
        );
        assert!(app.store.vouchers(company).unwrap().is_empty());
    }

    #[test]
    fn prepared_invoice_keeps_receipt_ledger_defaults() {
        let mut store = balance::Store::memory().unwrap();
        let company = store.create_company("Stock note invoice").unwrap();
        store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let purchases = store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let assets = store
            .create_ledger(company, "Equipment", Category::Asset)
            .unwrap();
        store.create_stock_item(company, "Part", "pcs").unwrap();
        let xml = br#"<ENVELOPE><BODY><DATA><MESSAGE><VOUCHER>
            <DATE>20260401</DATE><VOUCHERTYPENAME>Receipt Note</VOUCHERTYPENAME>
            <VOUCHERNUMBER>RN1</VOUCHERNUMBER><PARTYLEDGERNAME>Supplier</PARTYLEDGERNAME>
            <LEDGERENTRIES.LIST><LEDGERNAME>Purchases</LEDGERNAME></LEDGERENTRIES.LIST>
            <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY><RATE>10/pcs</RATE>
                <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>1 pcs</ACTUALQTY></BATCHALLOCATIONS.LIST>
                <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>1 pcs</ACTUALQTY>
                    <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Equipment</LEDGERNAME></ACCOUNTINGALLOCATIONS.LIST>
                </BATCHALLOCATIONS.LIST>
            </INVENTORYENTRIES.LIST>
            </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#;
        let plan = store.preview_xml_import(company, xml).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let note = store.stock_notes(company, None).unwrap().remove(0);
        let mut app = App::new(store, Default::default());
        app.prepare_stock_notes_invoice(company, std::iter::once(&note))
            .unwrap();
        assert_eq!(app.invoice_trade, Some(purchases));
        assert_eq!(
            app.invoice_lines
                .iter()
                .map(|line| line.trade_ledger)
                .collect::<Vec<_>>(),
            [None, Some(assets)]
        );
        assert!(app.invoice_source == Some(InvoiceSource::StockNotes));
    }
}
