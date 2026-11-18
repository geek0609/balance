use super::{
    money, pick_account, pick_batch, pick_location, pick_stock, App, InvoiceAllocationForm,
    InvoiceLineForm, InvoiceSource,
};
use balance::inventory::{format_quantity, parse_quantity, InvoiceKind, StockItem};
use balance::orders::{OrderAmendmentDraft, OrderDetail, OrderDraft, OrderLine, OrderStatus};
use balance::stock_allocation::{StockAllocationDraft, StockAllocationPlan};
use balance::stock_notes::note_label;
use balance::stock_reservations::StockReservationDraft;
use balance::{format_amount, parse_amount, Category, Error, Result};
use eframe::egui;
use std::collections::HashSet;

#[derive(Default)]
struct LineForm {
    item: Option<i64>,
    quantity: String,
    price: String,
}

impl LineForm {
    fn has_content(&self) -> bool {
        self.item.is_some() || !self.quantity.trim().is_empty() || !self.price.trim().is_empty()
    }

    fn draft(&self) -> Result<OrderLine> {
        Ok(OrderLine {
            item_id: self
                .item
                .ok_or_else(|| Error::Validation("Select an item for every line".into()))?,
            quantity: parse_quantity(&self.quantity)?,
            unit_price: parse_amount(&self.price)?,
        })
    }
}

fn order_line_fields(ui: &mut egui::Ui, lines: &mut Vec<LineForm>, items: &[StockItem]) {
    let mut remove = None;
    for (index, line) in lines.iter_mut().enumerate() {
        ui.horizontal_wrapped(|ui| {
            pick_stock(ui, ("order-item", index), "Item", items, &mut line.item);
            ui.add(
                egui::TextEdit::singleline(&mut line.quantity)
                    .hint_text("Total quantity")
                    .desired_width(100.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut line.price)
                    .hint_text("Unit price")
                    .desired_width(80.0),
            );
            if ui.button("Remove").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        lines.remove(index);
    }
    if ui.button("Add item").clicked() {
        lines.push(LineForm::default());
    }
}

struct CreateOrderForm {
    quotation: bool,
    kind: InvoiceKind,
    date: String,
    expected_on: String,
    valid_through: String,
    party: Option<i64>,
    reference: String,
    notes: String,
    lines: Vec<LineForm>,
}

impl Default for CreateOrderForm {
    fn default() -> Self {
        Self {
            quotation: false,
            kind: InvoiceKind::Sale,
            date: String::new(),
            expected_on: String::new(),
            valid_through: String::new(),
            party: None,
            reference: String::new(),
            notes: String::new(),
            lines: vec![LineForm::default()],
        }
    }
}

impl CreateOrderForm {
    fn has_content(&self) -> bool {
        self.party.is_some()
            || [
                &self.reference,
                &self.date,
                &self.expected_on,
                &self.valid_through,
                &self.notes,
            ]
            .iter()
            .any(|field| !field.trim().is_empty())
            || self.lines.iter().any(LineForm::has_content)
    }

    fn draft(&self) -> Result<OrderDraft> {
        let party_ledger_id = self
            .party
            .ok_or_else(|| Error::Validation("Select a party ledger".into()))?;
        let lines = self
            .lines
            .iter()
            .map(LineForm::draft)
            .collect::<Result<_>>()?;
        Ok(OrderDraft {
            kind: self.kind,
            party_ledger_id,
            reference: self.reference.clone(),
            date: self.date.clone(),
            expected_on: (!self.expected_on.trim().is_empty()).then(|| self.expected_on.clone()),
            notes: self.notes.clone(),
            lines,
        })
    }
}

struct OrderAmendmentForm {
    order: i64,
    base_revision: Option<i64>,
    reference: String,
    expected_on: String,
    notes: String,
    lines: Vec<LineForm>,
    reason: String,
}

impl OrderAmendmentForm {
    fn new(detail: &OrderDetail) -> Self {
        Self {
            order: detail.summary.id,
            base_revision: detail.summary.revision,
            reference: detail.summary.reference.clone(),
            expected_on: detail.summary.expected_on.clone().unwrap_or_default(),
            notes: detail.summary.notes.clone(),
            lines: detail
                .lines
                .iter()
                .map(|row| LineForm {
                    item: Some(row.line.item_id),
                    quantity: format_quantity(row.line.quantity.into()),
                    price: format_amount(row.line.unit_price),
                })
                .collect(),
            reason: String::new(),
        }
    }

    fn draft(&self) -> Result<OrderAmendmentDraft> {
        Ok(OrderAmendmentDraft {
            base_revision: self.base_revision,
            reference: self.reference.clone(),
            expected_on: (!self.expected_on.trim().is_empty()).then(|| self.expected_on.clone()),
            notes: self.notes.clone(),
            lines: self
                .lines
                .iter()
                .map(LineForm::draft)
                .collect::<Result<_>>()?,
            reason: self.reason.clone(),
        })
    }
}

#[derive(Default)]
struct QuotationForm {
    selected: Option<i64>,
    order_reference: String,
    order_date: String,
    expected_on: String,
    close_reason: String,
    document_path: String,
}

#[derive(Default)]
struct LineClosureForm {
    item: Option<i64>,
    date: String,
    quantity: String,
    reason: String,
}

#[derive(Default)]
struct LineReopenForm {
    closure: Option<i64>,
    date: String,
    reason: String,
}

#[derive(Default)]
struct StockReservationForm {
    item: Option<i64>,
    location: Option<i64>,
    batch: Option<i64>,
    quantity: String,
    reason: String,
}

#[derive(Default)]
struct StockReleaseForm {
    id: Option<i64>,
    quantity: String,
    reason: String,
}

#[derive(Default)]
struct StockAllocationForm {
    item: Option<i64>,
    location: Option<i64>,
    batch: Option<i64>,
    maximum: String,
    expected_through: String,
    reason: String,
    plan: Option<StockAllocationPlan>,
}

#[derive(Default)]
pub(super) struct OrderScreen {
    form: CreateOrderForm,
    kind: Option<InvoiceKind>,
    status: Option<OrderStatus>,
    search: String,
    selected: Option<i64>,
    amendment: Option<OrderAmendmentForm>,
    close_reason: String,
    line_closure: LineClosureForm,
    line_reopen: LineReopenForm,
    reservation: StockReservationForm,
    stock_release: StockReleaseForm,
    allocation: StockAllocationForm,
    replenishment_items: HashSet<i64>,
    document_path: String,
    schedule_kind: Option<InvoiceKind>,
    schedule_through: String,
    quotation: QuotationForm,
}

impl OrderScreen {
    pub(super) fn amending_order(&self) -> bool {
        self.amendment.is_some()
    }

    pub(super) fn select(&mut self, order: i64) {
        self.selected = Some(order);
        self.close_reason.clear();
        self.line_closure = LineClosureForm::default();
        self.line_reopen = LineReopenForm::default();
        self.reservation = StockReservationForm::default();
        self.stock_release = StockReleaseForm::default();
    }
}

impl App {
    pub(super) fn orders_page(&mut self, ui: &mut egui::Ui, company: i64) {
        if self.order_screen.amending_order() {
            if let Err(error) = self.order_amendment_form(ui, company) {
                self.failure(error);
            }
            return;
        }
        crate::theme::section(ui, "Sales and purchase orders");
        let prepared = egui::CollapsingHeader::new("Replenishment planning")
            .show(ui, |ui| match self.replenishment_view(ui, company) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.failure(error);
                    false
                }
            })
            .body_returned
            .unwrap_or(false);
        egui::CollapsingHeader::new("Create order or quotation")
            .open(prepared.then_some(true))
            .show(ui, |ui| self.create_order_form(ui, company));
        egui::CollapsingHeader::new("Quotations").show(ui, |ui| {
            if let Err(error) = self.quotation_register(ui, company) {
                self.failure(error);
            }
        });
        egui::CollapsingHeader::new("Stock availability")
            .show(ui, |ui| self.order_availability_view(ui, company));
        egui::CollapsingHeader::new("Pending order schedule")
            .show(ui, |ui| self.pending_order_schedule_view(ui, company));
        egui::CollapsingHeader::new("Allocate stock to sales orders").show(ui, |ui| {
            if let Err(error) = self.stock_allocation_view(ui, company) {
                self.failure(error);
            }
        });
        ui.separator();
        if let Some(selected) = self.order_screen.selected {
            match self.store.order_detail(company, selected) {
                Ok(detail) => self.order_detail_view(ui, company, &detail),
                Err(error) => self.failure(error),
            }
            ui.separator();
        }
        crate::theme::section(ui, "Order register");
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("order-kind-filter")
                .selected_text(
                    self.order_screen
                        .kind
                        .map(InvoiceKind::as_str)
                        .unwrap_or("All types"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.order_screen.kind, None, "All types");
                    for kind in InvoiceKind::ALL {
                        ui.selectable_value(&mut self.order_screen.kind, Some(kind), kind.as_str());
                    }
                });
            egui::ComboBox::from_id_salt("order-status-filter")
                .selected_text(
                    self.order_screen
                        .status
                        .map(OrderStatus::label)
                        .unwrap_or("All statuses"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.order_screen.status, None, "All statuses");
                    for status in OrderStatus::ALL {
                        ui.selectable_value(
                            &mut self.order_screen.status,
                            Some(status),
                            status.label(),
                        );
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.order_screen.search)
                    .hint_text("Reference or party"),
            );
        });
        let orders = match self.store.orders(company) {
            Ok(orders) => orders,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let search = self.order_screen.search.trim().to_lowercase();
        let orders = orders
            .into_iter()
            .filter(|order| {
                self.order_screen.kind.is_none_or(|kind| order.kind == kind)
                    && self
                        .order_screen
                        .status
                        .is_none_or(|status| order.status == status)
                    && (search.is_empty()
                        || order.reference.to_lowercase().contains(&search)
                        || order.party.to_lowercase().contains(&search))
            })
            .collect::<Vec<_>>();
        if orders.is_empty() {
            ui.label("No matching orders.");
        }
        for order in orders {
            let caption = format!(
                "{} | {} {} | {} | {}",
                order.date,
                order.kind.as_str(),
                order.reference,
                order.party,
                order.status.label()
            );
            if ui
                .selectable_label(self.order_screen.selected == Some(order.id), caption)
                .clicked()
            {
                self.order_screen.select(order.id);
            }
        }
    }

    fn pending_order_schedule_view(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("pending-order-schedule-kind")
                .selected_text(
                    self.order_screen
                        .schedule_kind
                        .map(InvoiceKind::as_str)
                        .unwrap_or("All types"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.order_screen.schedule_kind, None, "All types");
                    for kind in InvoiceKind::ALL {
                        ui.selectable_value(
                            &mut self.order_screen.schedule_kind,
                            Some(kind),
                            kind.as_str(),
                        );
                    }
                });
            ui.label("Expected through");
            ui.add(
                egui::TextEdit::singleline(&mut self.order_screen.schedule_through)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
        });
        ui.small(
            "Shows current unfulfilled lines. Clear the date filter to include undated orders.",
        );
        self.report_export_path_field(ui);
        let kind = self.order_screen.schedule_kind;
        let date = self.order_screen.schedule_through.trim();
        let through = (!date.is_empty()).then_some(date);
        let rows = match self.store.pending_order_schedule(company, kind, through) {
            Ok(rows) => rows,
            Err(error) => {
                ui.label(error.to_string());
                return;
            }
        };
        if ui.button("Export schedule as CSV").clicked() {
            match self.store.export_pending_order_schedule_csv(
                company,
                kind,
                through,
                self.report_export_path.trim(),
            ) {
                Ok(()) => self.message = "Pending order schedule exported".into(),
                Err(error) => self.failure(error),
            }
        }
        if rows.is_empty() {
            ui.label("No pending lines match the filters.");
            return;
        }
        egui::Grid::new("pending-order-schedule")
            .striped(true)
            .show(ui, |ui| {
                for title in ["Expected", "Order", "Party", "Item", "Remaining", ""] {
                    ui.strong(title);
                }
                ui.end_row();
                for line in rows {
                    ui.label(
                        line.expected_on
                            .map(|date| date.to_string())
                            .unwrap_or_else(|| "Undated".into()),
                    );
                    ui.label(format!("{} {}", line.kind.as_str(), line.reference));
                    ui.label(line.party);
                    ui.label(line.item);
                    ui.label(format!(
                        "{} {}",
                        format_quantity(i128::from(line.remaining)),
                        line.unit
                    ));
                    ui.push_id((line.order_id, line.item_id), |ui| {
                        if ui.button("View order").clicked() {
                            self.order_screen.select(line.order_id);
                        }
                    });
                    ui.end_row();
                }
            });
    }

    fn order_availability_view(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.small("Uses stock movements and open order quantities across all saved dates, including future dates. The projection adds open purchases and subtracts open sales from on-hand stock.");
        self.report_export_path_field(ui);
        if ui.button("Export availability as CSV").clicked() {
            match self
                .store
                .export_order_availability_csv(company, self.report_export_path.trim())
            {
                Ok(()) => self.message = "Stock availability report exported".into(),
                Err(error) => self.failure(error),
            }
        }
        let availability = match self.store.order_availability(company) {
            Ok(rows) => rows,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        if availability.is_empty() {
            ui.label("Create a stock item to see availability.");
            return;
        }
        egui::Grid::new("order-stock-availability")
            .striped(true)
            .show(ui, |ui| {
                for label in [
                    "Item",
                    "Unit",
                    "On hand",
                    "In transit",
                    "Reserved",
                    "Unreserved",
                    "Open sales",
                    "Open purchases",
                    "After sales",
                    "Projected",
                ] {
                    ui.strong(label);
                }
                ui.end_row();
                for row in availability {
                    ui.label(&row.item.name);
                    ui.label(&row.item.unit);
                    for quantity in [
                        row.on_hand,
                        row.in_transit,
                        row.reserved,
                        row.unreserved(),
                        row.open_sales,
                        row.open_purchases,
                        row.after_sales(),
                        row.projected(),
                    ] {
                        ui.label(format_quantity(quantity));
                    }
                    ui.end_row();
                }
            });
    }

    fn replenishment_view(&mut self, ui: &mut egui::Ui, company: i64) -> Result<bool> {
        ui.small("Projected stock uses all saved dates. Suggestions use whole reorder lots.");
        self.report_export_path_field(ui);
        if ui.button("Export replenishment CSV").clicked() {
            self.store
                .export_replenishment_csv(company, self.report_export_path.trim())?;
            self.message = "Replenishment suggestions exported".into();
        }
        let suggestions = self.store.replenishment_suggestions(company)?;
        let selected = &mut self.order_screen.replenishment_items;
        selected.retain(|item| suggestions.iter().any(|row| row.stock.item.id == *item));
        if suggestions.is_empty() {
            ui.label("No items have reached their projected reorder level.");
            return Ok(false);
        }
        egui::Grid::new("replenishment-planning")
            .striped(true)
            .show(ui, |ui| {
                for label in [
                    "Item",
                    "On hand",
                    "Open sales",
                    "Open purchases",
                    "Projected",
                    "Reorder level",
                    "Suggested",
                ] {
                    ui.strong(label);
                }
                ui.end_row();
                for row in &suggestions {
                    let item = &row.stock.item;
                    let mut checked = selected.contains(&item.id);
                    if ui
                        .checkbox(&mut checked, format!("{} ({})", item.name, item.unit))
                        .changed()
                    {
                        if checked {
                            selected.insert(item.id);
                        } else {
                            selected.remove(&item.id);
                        }
                    }
                    for quantity in [
                        row.stock.on_hand,
                        row.stock.open_sales,
                        row.stock.open_purchases,
                        row.stock.projected(),
                        i128::from(row.rule.level),
                        row.suggested_quantity,
                    ] {
                        ui.label(format_quantity(quantity));
                    }
                    ui.end_row();
                }
            });
        let form = &mut self.order_screen.form;
        let can_prepare =
            !form.quotation && (form.kind == InvoiceKind::Purchase || !form.has_content());
        let additions: Vec<_> = suggestions
            .iter()
            .filter(|row| {
                selected.contains(&row.stock.item.id)
                    && !form
                        .lines
                        .iter()
                        .any(|line| line.item == Some(row.stock.item.id))
            })
            .map(|row| LineForm {
                item: Some(row.stock.item.id),
                quantity: format_quantity(row.suggested_quantity),
                price: String::new(),
            })
            .collect();
        let prepare = ui.add_enabled(
            can_prepare && !additions.is_empty(),
            egui::Button::new("Add to purchase order draft"),
        );
        let prepare = if can_prepare {
            prepare
        } else {
            prepare.on_disabled_hover_text(
                "Save or clear the current sales order or quotation draft first.",
            )
        };
        if prepare.clicked() {
            form.kind = InvoiceKind::Purchase;
            form.lines.retain(LineForm::has_content);
            for line in &additions {
                if let Some(item) = line.item {
                    selected.remove(&item);
                }
            }
            form.lines.extend(additions);
            self.message = "Replenishment lines added to the purchase order draft".into();
            return Ok(true);
        }
        Ok(false)
    }

    fn create_order_form(&mut self, ui: &mut egui::Ui, company: i64) {
        let ledgers = match self.store.ledgers(company) {
            Ok(ledgers) => ledgers
                .into_iter()
                .filter(|ledger| matches!(ledger.category, Category::Asset | Category::Liability))
                .collect::<Vec<_>>(),
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let items = match self.store.stock_items(company) {
            Ok(items) => items,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let form = &mut self.order_screen.form;
        ui.checkbox(&mut form.quotation, "Save as quotation");
        egui::ComboBox::from_id_salt("order-kind")
            .selected_text(form.kind.as_str())
            .show_ui(ui, |ui| {
                for kind in InvoiceKind::ALL {
                    ui.selectable_value(&mut form.kind, kind, kind.as_str());
                }
            });
        pick_account(ui, "order-party", "Party", &ledgers, &mut form.party, false);
        egui::Grid::new("order-fields").show(ui, |ui| {
            let date_label = if form.quotation {
                "Quotation date (YYYY-MM-DD)"
            } else {
                "Order date (YYYY-MM-DD)"
            };
            for (label, value) in [
                ("Reference", &mut form.reference),
                (date_label, &mut form.date),
                ("Expected date (optional)", &mut form.expected_on),
                ("Notes", &mut form.notes),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
            if form.quotation {
                ui.label("Valid through (optional)");
                ui.add(egui::TextEdit::singleline(&mut form.valid_through).hint_text("YYYY-MM-DD"));
                ui.end_row();
            }
        });
        order_line_fields(ui, &mut form.lines, &items);
        let quotation = form.quotation;
        if ui
            .button(if quotation {
                "Save quotation"
            } else {
                "Save order"
            })
            .clicked()
        {
            let result = form.draft().and_then(|draft| {
                if quotation {
                    let expiry = form.valid_through.trim();
                    self.store.create_quotation(
                        company,
                        &draft,
                        (!expiry.is_empty()).then_some(expiry),
                    )
                } else {
                    self.store.create_order(company, &draft)
                }
            });
            match result {
                Ok(id) if quotation => {
                    self.order_screen.quotation = QuotationForm {
                        selected: Some(id),
                        order_reference: form.reference.trim().to_owned(),
                        expected_on: form.expected_on.trim().to_owned(),
                        ..QuotationForm::default()
                    };
                    self.order_screen.form = CreateOrderForm::default();
                    self.message = format!("Quotation {id} saved");
                }
                Ok(order) => {
                    self.order_screen.form = CreateOrderForm::default();
                    self.order_screen.select(order);
                    self.message = format!("Order {order} saved");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn quotation_register(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        let quotes = self.store.quotations(company)?;
        if quotes.is_empty() {
            ui.label("No quotations saved.");
            return Ok(());
        }
        let form = &mut self.order_screen.quotation;
        let mut selected = form.selected;
        let picked = quotes.iter().find(|quote| Some(quote.id) == selected);
        egui::ComboBox::from_label("Quotation")
            .selected_text(
                picked
                    .map(|quote| quote.reference.as_str())
                    .unwrap_or("Select quotation"),
            )
            .show_ui(ui, |ui| {
                for quote in &quotes {
                    ui.selectable_value(
                        &mut selected,
                        Some(quote.id),
                        format!(
                            "{} {} · {} · {}",
                            quote.kind.as_str(),
                            quote.reference,
                            quote.party,
                            quote.status()
                        ),
                    );
                }
            });
        let Some(quote) = quotes.iter().find(|quote| Some(quote.id) == selected) else {
            return Ok(());
        };
        if form.selected != selected {
            *form = QuotationForm {
                selected,
                order_reference: quote.reference.clone(),
                expected_on: quote.expected_on.clone().unwrap_or_default(),
                ..QuotationForm::default()
            };
        }
        ui.label(format!(
            "{} · {} · {}",
            quote.party,
            quote.date,
            quote.status()
        ));
        if let Some(expiry) = &quote.valid_through {
            ui.label(format!("Valid through {expiry}"));
        }
        if let Some(expected) = &quote.expected_on {
            ui.label(format!("Expected {expected}"));
        }
        if !quote.notes.is_empty() {
            ui.label(&quote.notes);
        }
        let items = self.store.stock_items(company)?;
        let lines = self.store.quotation_lines(company, quote.id)?;
        egui::Grid::new("quotation-lines")
            .striped(true)
            .show(ui, |ui| {
                for heading in ["Item", "Quantity", "Unit price"] {
                    ui.strong(heading);
                }
                ui.end_row();
                for line in lines {
                    let item = items.iter().find(|item| item.id == line.item_id);
                    ui.label(
                        item.map(|item| item.name.as_str())
                            .unwrap_or("Unavailable item"),
                    );
                    ui.label(format!(
                        "{} {}",
                        format_quantity(i128::from(line.quantity)),
                        item.map(|item| item.unit.as_str()).unwrap_or(""),
                    ));
                    ui.label(format_amount(line.unit_price));
                    ui.end_row();
                }
            });
        ui.horizontal(|ui| {
            ui.label("Document path");
            ui.add(
                egui::TextEdit::singleline(&mut form.document_path)
                    .hint_text("Absolute path to a new .html file"),
            );
        });
        if ui.button("Export quotation for printing").clicked() {
            self.store
                .export_quotation_html(company, quote.id, form.document_path.trim())?;
            self.message = "Quotation document exported".into();
        }
        if let Some(order) = quote.order_id {
            if let Some(converted) = &quote.converted_at {
                ui.label(format!("Converted {converted} UTC"));
            }
            if ui.button(format!("View order #{order}")).clicked() {
                self.order_screen.select(order);
            }
            return Ok(());
        }
        if let Some(closed) = &quote.closed_at {
            ui.label(format!("Closed {closed} UTC"));
            if let Some(reason) = &quote.close_reason {
                ui.label(reason);
            }
            return Ok(());
        }

        egui::Grid::new("quotation-conversion").show(ui, |ui| {
            for (label, value) in [
                ("Order reference", &mut form.order_reference),
                ("Order date (YYYY-MM-DD)", &mut form.order_date),
                ("Expected date (optional)", &mut form.expected_on),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        if ui.button("Convert to order").clicked() {
            let delivery = form.expected_on.trim();
            let order = self.store.convert_quotation(
                company,
                quote.id,
                &form.order_reference,
                &form.order_date,
                (!delivery.is_empty()).then_some(delivery),
            )?;
            self.order_screen.select(order);
            self.message = format!("Order {order} created from quotation {}", quote.reference);
            return Ok(());
        }

        ui.horizontal(|ui| {
            ui.label("Close reason");
            ui.text_edit_singleline(&mut form.close_reason);
        });
        if ui.button("Close quotation").clicked() {
            self.store
                .close_quotation(company, quote.id, &form.close_reason)?;
            form.close_reason.clear();
            self.message = "Quotation closed".into();
        }
        Ok(())
    }

    fn order_detail_view(&mut self, ui: &mut egui::Ui, company: i64, detail: &OrderDetail) {
        let order = &detail.summary;
        crate::theme::section(
            ui,
            format!(
                "{} order {} | {}",
                order.kind.as_str(),
                order.reference,
                order.status.label()
            ),
        );
        match self.store.order_quotation(company, order.id) {
            Ok(Some(reference)) => {
                ui.label(format!("Quotation {reference}"));
            }
            Ok(None) => {}
            Err(error) => self.failure(error),
        }
        if ui.button("Hide order details").clicked() {
            self.order_screen.selected = None;
            self.order_screen.close_reason.clear();
        }
        if order.closed_at.is_none() && ui.button("Amend order").clicked() {
            self.order_screen.amendment = Some(OrderAmendmentForm::new(detail));
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.order_screen.document_path)
                    .hint_text("Absolute path to a new .html file"),
            );
            if ui.button("Export printable order").clicked() {
                match self.store.export_order_html(
                    company,
                    order.id,
                    self.order_screen.document_path.trim(),
                ) {
                    Ok(()) => self.message = "Order document created".into(),
                    Err(error) => self.failure(error),
                }
            }
        });
        ui.label(format!("{} | {}", order.date, order.party));
        if let Some(date) = &order.expected_on {
            ui.label(format!("Expected {date}"));
        }
        if !order.notes.is_empty() {
            ui.label(&order.notes);
        }
        egui::Grid::new(("order-detail", order.id))
            .striped(true)
            .show(ui, |ui| {
                for label in [
                    "Item",
                    "Ordered",
                    "Fulfilled",
                    "Invoiced",
                    "Pre-closed",
                    "Unfulfilled",
                    "Unit price",
                ] {
                    ui.strong(label);
                }
                ui.end_row();
                for row in &detail.lines {
                    ui.label(&row.item);
                    ui.label(format!(
                        "{} {}",
                        format_quantity(row.line.quantity.into()),
                        row.unit
                    ));
                    ui.label(format_quantity(row.fulfilled.into()));
                    ui.label(format_quantity(row.billed.into()));
                    ui.label(format_quantity(row.preclosed.into()));
                    ui.label(format_quantity(row.unfulfilled().into()));
                    ui.label(format_amount(row.line.unit_price));
                    ui.end_row();
                }
            });
        ui.label(format!(
            "Order value before tax and adjustments {}",
            money(detail.total)
        ));
        if !detail.invoices.is_empty() {
            ui.strong("Linked invoices");
            for invoice in &detail.invoices {
                let mut label = format!(
                    "{} | {} (invoice {})",
                    invoice.date, invoice.reference, invoice.id
                );
                if let Some(date) = &invoice.reversal_date {
                    label.push_str(&format!(" | Reversed {date}"));
                }
                ui.label(label);
            }
        }
        if let (Some(at), Some(reason)) = (&order.closed_at, &order.close_reason) {
            ui.label(format!("Closed {at} UTC | {reason}"));
        }
        if order.revision.is_some() {
            egui::CollapsingHeader::new("Amendment history")
                .id_salt(("order-amendment-history", order.id))
                .show(ui, |ui| {
                    if let Err(error) = self.order_amendment_history(ui, company, order.id) {
                        self.failure(error);
                    }
                });
        }
        if order.status.has_pending() || !detail.line_closures.is_empty() {
            egui::CollapsingHeader::new("Line closures")
                .id_salt(order.id)
                .show(ui, |ui| self.order_line_closures(ui, company, detail));
        }
        if (order.kind == InvoiceKind::Sale && order.status.has_pending())
            || !detail.reservations.is_empty()
        {
            egui::CollapsingHeader::new("Stock reservations")
                .id_salt(("order-stock-reservations", order.id))
                .show(ui, |ui| {
                    if let Err(error) = self.order_stock_reservations(ui, company, detail) {
                        self.failure(error);
                    }
                });
        }
        match self.store.stock_notes(company, Some(order.id)) {
            Ok(notes) => {
                for note in notes {
                    let label = format!(
                        "{} {} · {} · {}",
                        note_label(note.kind),
                        note.reference,
                        note.date,
                        note.status()
                    );
                    if ui.link(label).clicked() {
                        self.open_stock_note(note.id);
                    }
                }
            }
            Err(error) => self.failure(error),
        }
        if order.status.has_pending() {
            if ui
                .button(format!(
                    "Prepare {} (replace note draft)",
                    note_label(order.kind).to_lowercase()
                ))
                .clicked()
            {
                self.prepare_order_stock_note(detail);
            }
            if ui
                .add_enabled(
                    !self.invoice_draft_in_use(),
                    egui::Button::new("Prepare invoice (replace draft)"),
                )
                .clicked()
            {
                self.prepare_order_invoice(detail);
            }
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.order_screen.close_reason)
                        .hint_text("Reason for closing"),
                );
                if ui
                    .add_enabled(
                        self.invoice_source != Some(InvoiceSource::Order(order.id))
                            && !self.order_screen.close_reason.trim().is_empty(),
                        egui::Button::new("Close unfulfilled quantities"),
                    )
                    .clicked()
                {
                    match self
                        .store
                        .close_order(company, order.id, &self.order_screen.close_reason)
                    {
                        Ok(()) => {
                            self.order_screen.close_reason.clear();
                            self.message = format!("Order {} closed", order.id);
                        }
                        Err(error) => self.failure(error),
                    }
                }
            });
        }
    }

    fn order_amendment_form(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        let Some(form) = &mut self.order_screen.amendment else {
            return Ok(());
        };
        crate::theme::section(ui, "Amend order");
        egui::Grid::new("order-amendment-fields").show(ui, |ui| {
            for (label, value) in [
                ("Reference", &mut form.reference),
                ("Expected date (optional)", &mut form.expected_on),
                ("Notes", &mut form.notes),
                ("Reason for amendment", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let items = self.store.stock_items(company)?;
        order_line_fields(ui, &mut form.lines, &items);
        let (save, cancel) = ui
            .horizontal(|ui| {
                (
                    ui.button("Save amendment").clicked(),
                    ui.button("Cancel").clicked(),
                )
            })
            .inner;
        if save {
            let order = form.order;
            self.store.amend_order(company, order, &form.draft()?)?;
            self.order_screen.amendment = None;
            self.order_screen.select(order);
            self.message = "Order amendment saved".into();
        } else if cancel {
            self.order_screen.amendment = None;
        }
        Ok(())
    }

    fn order_amendment_history(&self, ui: &mut egui::Ui, company: i64, order: i64) -> Result<()> {
        for revision in self.store.order_revisions(company, order)? {
            egui::CollapsingHeader::new(format!(
                "{} UTC · {}",
                revision.amended_at, revision.reason
            ))
            .id_salt(("order-amendment", revision.id))
            .show(ui, |ui| -> Result<()> {
                ui.label(format!("Previous reference {}", revision.reference));
                if let Some(expected) = &revision.expected_on {
                    ui.label(format!("Previously expected {expected}"));
                }
                if !revision.notes.is_empty() {
                    ui.label(&revision.notes);
                }
                let lines = self
                    .store
                    .order_revision_lines(company, order, revision.id)?;
                egui::Grid::new(("order-amendment-lines", revision.id))
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Item");
                        ui.strong("Previous quantity");
                        ui.strong("Previous unit price");
                        ui.end_row();
                        for line in lines {
                            ui.label(&line.item);
                            ui.label(format!(
                                "{} {}",
                                format_quantity(line.values.quantity.into()),
                                line.unit
                            ));
                            ui.label(format_amount(line.values.unit_price));
                            ui.end_row();
                        }
                    });
                Ok(())
            })
            .body_returned
            .transpose()?;
        }
        Ok(())
    }

    fn stock_allocation_view(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        let form = &mut self.order_screen.allocation;
        let items = self.store.stock_items(company)?;
        let locations = self.store.stock_locations(company)?;
        ui.horizontal_wrapped(|ui| -> Result<()> {
            let previous = (form.item, form.location, form.batch);
            pick_stock(ui, "allocate-stock-item", "Item", &items, &mut form.item);
            if previous.0 != form.item {
                form.batch = None;
            }
            pick_location(
                ui,
                "allocate-stock-location",
                "Location",
                &locations,
                &mut form.location,
                false,
            );
            if let Some(item) = form.item {
                let batches = self.store.stock_batches(company, item)?;
                pick_batch(ui, "allocate-stock-batch", &batches, &mut form.batch);
            }
            if previous != (form.item, form.location, form.batch) {
                form.plan = None;
            }
            Ok(())
        })
        .inner?;
        ui.horizontal_wrapped(|ui| {
            ui.label("Maximum quantity (optional)");
            let maximum_changed = ui
                .add(egui::TextEdit::singleline(&mut form.maximum).desired_width(95.0))
                .changed();
            ui.label("Expected through (optional)");
            let date_changed = ui
                .add(
                    egui::TextEdit::singleline(&mut form.expected_through)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(115.0),
                )
                .changed();
            if maximum_changed || date_changed {
                form.plan = None;
            }
        });
        let preview = ui.add_enabled(
            form.item.is_some() && form.location.is_some(),
            egui::Button::new("Preview allocation"),
        );
        if let (true, Some(item_id), Some(location_id)) =
            (preview.clicked(), form.item, form.location)
        {
            form.plan = None;
            let maximum = (!form.maximum.trim().is_empty())
                .then(|| parse_quantity(&form.maximum))
                .transpose()?;
            form.plan = Some(
                self.store.preview_stock_allocation(
                    company,
                    &StockAllocationDraft {
                        item_id,
                        location_id,
                        batch_id: form.batch,
                        maximum,
                        expected_through: (!form.expected_through.trim().is_empty())
                            .then(|| form.expected_through.clone()),
                    },
                )?,
            );
        }
        let Some(plan) = &form.plan else {
            return Ok(());
        };
        let unit = items
            .iter()
            .find(|item| Some(item.id) == form.item)
            .map(|item| item.unit.as_str())
            .unwrap_or("");
        ui.label(format!(
            "{} {unit} available to reserve",
            format_quantity(plan.available())
        ));
        if plan.allocations().is_empty() {
            ui.label("No stock can be allocated with this selection.");
            return Ok(());
        }
        egui::Grid::new("stock-allocation-preview")
            .striped(true)
            .show(ui, |ui| {
                for label in [
                    "Sales order",
                    "Party",
                    "Order date",
                    "Expected",
                    "Unreserved",
                    "To reserve",
                ] {
                    ui.strong(label);
                }
                ui.end_row();
                for row in plan.allocations() {
                    ui.label(&row.reference);
                    ui.label(&row.party);
                    ui.label(row.order_date.to_string());
                    ui.label(
                        row.expected_on
                            .map(|date| date.to_string())
                            .unwrap_or_else(|| "—".into()),
                    );
                    ui.label(format_quantity(row.unreserved.into()));
                    ui.label(format_quantity(row.quantity.into()));
                    ui.end_row();
                }
            });
        let apply = ui
            .horizontal_wrapped(|ui| {
                ui.label("Reservation reason");
                ui.text_edit_singleline(&mut form.reason);
                ui.add_enabled(
                    !form.reason.trim().is_empty(),
                    egui::Button::new("Apply reservations"),
                )
                .clicked()
            })
            .inner;
        if apply {
            let count = self
                .store
                .apply_stock_allocation(company, plan, &form.reason)?;
            form.plan = None;
            form.reason.clear();
            self.message = format!("Stock reserved on {count} order lines");
        }
        Ok(())
    }

    fn order_stock_reservations(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        detail: &OrderDetail,
    ) -> Result<()> {
        let order = &detail.summary;
        if order.status.has_pending() {
            let form = &mut self.order_screen.reservation;
            let locations = self.store.stock_locations(company)?;
            ui.horizontal_wrapped(|ui| -> Result<()> {
                let available = detail.lines.iter().filter_map(|line| {
                    let reserved: i64 = detail
                        .reservations
                        .iter()
                        .filter(|hold| hold.item_id == line.line.item_id)
                        .map(|hold| hold.remaining)
                        .sum();
                    let remaining = line.unfulfilled() - reserved;
                    (remaining > 0).then_some((line, remaining))
                });
                egui::ComboBox::from_id_salt(("reserve-order-item", order.id))
                    .selected_text(
                        available
                            .clone()
                            .find(|(line, _)| Some(line.line.item_id) == form.item)
                            .map(|(line, _)| line.item.as_str())
                            .unwrap_or("Select item"),
                    )
                    .show_ui(ui, |ui| {
                        for (line, quantity) in available {
                            let label = format!(
                                "{} · {} {} unreserved",
                                line.item,
                                format_quantity(quantity.into()),
                                line.unit
                            );
                            if ui
                                .selectable_value(&mut form.item, Some(line.line.item_id), label)
                                .clicked()
                            {
                                form.batch = None;
                                form.quantity = format_quantity(quantity.into());
                            }
                        }
                    });
                pick_location(
                    ui,
                    ("reserve-location", order.id),
                    "Location",
                    &locations,
                    &mut form.location,
                    false,
                );
                if let Some(item) = form.item {
                    pick_batch(
                        ui,
                        ("reserve-batch", order.id),
                        &self.store.stock_batches(company, item)?,
                        &mut form.batch,
                    );
                }
                ui.add(
                    egui::TextEdit::singleline(&mut form.quantity)
                        .hint_text("Quantity")
                        .desired_width(90.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut form.reason).hint_text("Reason for reserving"),
                );
                let save = ui.add_enabled(
                    form.item.is_some() && form.location.is_some(),
                    egui::Button::new("Reserve stock"),
                );
                if let (true, Some(item_id), Some(location_id)) =
                    (save.clicked(), form.item, form.location)
                {
                    self.store.reserve_order_stock(
                        company,
                        order.id,
                        &StockReservationDraft {
                            item_id,
                            location_id,
                            batch_id: form.batch,
                            quantity: parse_quantity(&form.quantity)?,
                            reason: form.reason.clone(),
                        },
                    )?;
                    *form = StockReservationForm::default();
                    self.message = "Stock reserved for this order".into();
                }
                Ok(())
            })
            .inner?;
        }
        for hold in &detail.reservations {
            egui::CollapsingHeader::new(format!(
                "{} · {} · {} · {} {} reserved",
                hold.item,
                hold.location,
                hold.batch.as_deref().unwrap_or("Unbatched"),
                format_quantity(hold.remaining.into()),
                hold.unit,
            ))
            .id_salt(("stock-reservation", hold.id))
            .show(ui, |ui| -> Result<()> {
                ui.label(format!(
                    "{} UTC · {} {} · {}",
                    hold.reserved_at,
                    format_quantity(hold.quantity.into()),
                    hold.unit,
                    hold.reason
                ));
                for event in self
                    .store
                    .stock_reservation_history(company, order.id, hold.id)?
                {
                    ui.label(format!(
                        "{} UTC · {} {} {} · {}",
                        event.released_at,
                        if event.fulfilled {
                            "Fulfilled"
                        } else {
                            "Released"
                        },
                        format_quantity(event.quantity.into()),
                        hold.unit,
                        event.reason
                    ));
                }
                let form = &mut self.order_screen.stock_release;
                if hold.remaining > 0
                    && form.id != Some(hold.id)
                    && ui.button("Release stock").clicked()
                {
                    *form = StockReleaseForm {
                        id: Some(hold.id),
                        quantity: format_quantity(hold.remaining.into()),
                        reason: String::new(),
                    };
                }
                if hold.remaining > 0 && form.id == Some(hold.id) {
                    ui.horizontal_wrapped(|ui| -> Result<()> {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.quantity)
                                .hint_text("Quantity")
                                .desired_width(90.0),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut form.reason)
                                .hint_text("Reason for releasing"),
                        );
                        if ui.button("Confirm release").clicked() {
                            self.store.release_order_stock(
                                company,
                                order.id,
                                hold.id,
                                parse_quantity(&form.quantity)?,
                                &form.reason,
                            )?;
                            *form = StockReleaseForm::default();
                            self.message = "Reserved stock released".into();
                        } else if ui.button("Cancel").clicked() {
                            *form = StockReleaseForm::default();
                        }
                        Ok(())
                    })
                    .inner?;
                }
                Ok(())
            })
            .body_returned
            .transpose()?;
        }
        Ok(())
    }

    fn order_line_closures(&mut self, ui: &mut egui::Ui, company: i64, detail: &OrderDetail) {
        let order = &detail.summary;
        if order.status.has_pending() {
            let pending = detail.lines.iter().filter(|line| line.unfulfilled() > 0);
            ui.horizontal_wrapped(|ui| {
                let form = &mut self.order_screen.line_closure;
                egui::ComboBox::from_id_salt(("preclose-order-item", order.id))
                    .selected_text(
                        pending
                            .clone()
                            .find(|line| Some(line.line.item_id) == form.item)
                            .map(|line| line.item.as_str())
                            .unwrap_or("Select item"),
                    )
                    .show_ui(ui, |ui| {
                        for line in pending {
                            let caption = format!(
                                "{} · {} {} pending",
                                line.item,
                                format_quantity(line.unfulfilled().into()),
                                line.unit,
                            );
                            if ui
                                .selectable_value(&mut form.item, Some(line.line.item_id), caption)
                                .clicked()
                            {
                                form.quantity = format_quantity(line.unfulfilled().into());
                            }
                        }
                    });
                ui.add(egui::TextEdit::singleline(&mut form.date).hint_text("YYYY-MM-DD"));
                ui.add(egui::TextEdit::singleline(&mut form.quantity).hint_text("Quantity"));
                ui.add(
                    egui::TextEdit::singleline(&mut form.reason).hint_text("Reason for closing"),
                );
                let save =
                    ui.add_enabled(form.item.is_some(), egui::Button::new("Pre-close quantity"));
                if let (true, Some(item)) = (save.clicked(), form.item) {
                    let result = parse_quantity(&form.quantity).and_then(|quantity| {
                        self.store.preclose_order_line(
                            company,
                            order.id,
                            item,
                            &form.date,
                            quantity,
                            &form.reason,
                        )
                    });
                    match result {
                        Ok(_) => {
                            *form = LineClosureForm::default();
                            self.message = "Order line quantity pre-closed".into();
                        }
                        Err(error) => self.failure(error),
                    }
                }
            });
        }
        for closure in &detail.line_closures {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{} · {} {} {} · {}",
                    closure.closed_on,
                    closure.item,
                    format_quantity(closure.quantity.into()),
                    closure.unit,
                    closure.reason,
                ));
                if let (Some(date), Some(reason)) = (&closure.reopened_on, &closure.reopen_reason) {
                    ui.label(format!("Reopened {date} · {reason}"));
                } else if order.closed_at.is_none() && ui.button("Reopen quantity").clicked() {
                    self.order_screen.line_reopen = LineReopenForm {
                        closure: Some(closure.id),
                        ..LineReopenForm::default()
                    };
                }
            });
            if self.order_screen.line_reopen.closure == Some(closure.id)
                && closure.reopened_on.is_none()
                && order.closed_at.is_none()
            {
                ui.push_id(closure.id, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let form = &mut self.order_screen.line_reopen;
                        ui.add(egui::TextEdit::singleline(&mut form.date).hint_text("YYYY-MM-DD"));
                        ui.add(
                            egui::TextEdit::singleline(&mut form.reason)
                                .hint_text("Reason for reopening"),
                        );
                        if ui.button("Confirm reopening").clicked() {
                            match self.store.reopen_order_line(
                                company,
                                order.id,
                                closure.id,
                                &form.date,
                                &form.reason,
                            ) {
                                Ok(()) => {
                                    *form = LineReopenForm::default();
                                    self.message = "Order line quantity reopened".into();
                                }
                                Err(error) => self.failure(error),
                            }
                        } else if ui.button("Cancel").clicked() {
                            *form = LineReopenForm::default();
                        }
                    });
                });
            }
        }
    }

    fn prepare_order_invoice(&mut self, detail: &OrderDetail) {
        let order = &detail.summary;
        let lines = detail
            .lines
            .iter()
            .filter(|line| line.unfulfilled() > 0)
            .map(|line| {
                let mut form = InvoiceLineForm::new();
                form.item = Some(line.line.item_id);
                form.quantity = format_quantity(line.unfulfilled().into());
                form.price = format_amount(line.line.unit_price);
                let allocations = detail.reserved_allocations(line.line.item_id);
                if let Some(first) = allocations.first() {
                    form.location = Some(first.location_id);
                    form.batch = first.batch_id;
                    let quantity: i128 = allocations.iter().map(|a| i128::from(a.quantity)).sum();
                    form.quantity = format_quantity(quantity);
                    form.extra_allocations = allocations
                        .iter()
                        .skip(1)
                        .map(|a| InvoiceAllocationForm {
                            location: Some(a.location_id),
                            batch: a.batch_id,
                            quantity: format_quantity(a.quantity.into()),
                        })
                        .collect();
                }
                form
            })
            .collect();
        self.prepare_linked_invoice(
            InvoiceSource::Order(order.id),
            order.kind,
            order.party_ledger_id,
            lines,
        );
    }
}
