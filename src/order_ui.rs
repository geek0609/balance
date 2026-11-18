use super::{money, pick_account, pick_stock, App, InvoiceLineForm, Page};
use balance::inventory::{format_quantity, parse_quantity, InvoiceKind};
use balance::orders::{OrderDetail, OrderDraft, OrderLine, OrderStatus};
use balance::{format_amount, parse_amount, Category, Error, Result};
use eframe::egui;

#[derive(Default)]
struct LineForm {
    item: Option<i64>,
    quantity: String,
    price: String,
}

struct CreateOrderForm {
    kind: InvoiceKind,
    date: String,
    expected_on: String,
    party: Option<i64>,
    reference: String,
    notes: String,
    lines: Vec<LineForm>,
}

impl Default for CreateOrderForm {
    fn default() -> Self {
        Self {
            kind: InvoiceKind::Sale,
            date: String::new(),
            expected_on: String::new(),
            party: None,
            reference: String::new(),
            notes: String::new(),
            lines: vec![LineForm::default()],
        }
    }
}

impl CreateOrderForm {
    fn draft(&self) -> Result<OrderDraft> {
        let party_ledger_id = self
            .party
            .ok_or_else(|| Error::Validation("Select a party ledger".into()))?;
        let lines = self
            .lines
            .iter()
            .map(|line| {
                Ok(OrderLine {
                    item_id: line
                        .item
                        .ok_or_else(|| Error::Validation("Select an item for every line".into()))?,
                    quantity: parse_quantity(&line.quantity)?,
                    unit_price: parse_amount(&line.price)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
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

#[derive(Default)]
pub(super) struct OrderScreen {
    form: CreateOrderForm,
    kind: Option<InvoiceKind>,
    status: Option<OrderStatus>,
    search: String,
    selected: Option<i64>,
    close_reason: String,
    schedule_kind: Option<InvoiceKind>,
    schedule_through: String,
}

impl OrderScreen {
    pub(super) fn select(&mut self, order: i64) {
        self.selected = Some(order);
        self.close_reason.clear();
    }
}

impl App {
    pub(super) fn orders_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Sales and purchase orders");
        egui::CollapsingHeader::new("Create order")
            .show(ui, |ui| self.create_order_form(ui, company));
        egui::CollapsingHeader::new("Stock availability")
            .show(ui, |ui| self.order_availability_view(ui, company));
        egui::CollapsingHeader::new("Pending order schedule")
            .show(ui, |ui| self.pending_order_schedule_view(ui, company));
        ui.separator();
        if let Some(selected) = self.order_screen.selected {
            match self.store.order_detail(company, selected) {
                Ok(detail) => self.order_detail_view(ui, company, &detail),
                Err(error) => self.failure(error),
            }
            ui.separator();
        }
        ui.heading("Order register");
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
        egui::ComboBox::from_id_salt("order-kind")
            .selected_text(form.kind.as_str())
            .show_ui(ui, |ui| {
                for kind in InvoiceKind::ALL {
                    ui.selectable_value(&mut form.kind, kind, kind.as_str());
                }
            });
        pick_account(ui, "order-party", "Party", &ledgers, &mut form.party, false);
        egui::Grid::new("order-fields").show(ui, |ui| {
            for (label, value) in [
                ("Reference", &mut form.reference),
                ("Order date (YYYY-MM-DD)", &mut form.date),
                ("Expected date (optional)", &mut form.expected_on),
                ("Notes", &mut form.notes),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let mut remove = None;
        for (index, line) in form.lines.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                pick_stock(ui, ("order-item", index), "Item", &items, &mut line.item);
                ui.add(
                    egui::TextEdit::singleline(&mut line.quantity)
                        .hint_text("Quantity")
                        .desired_width(80.0),
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
            form.lines.remove(index);
        }
        if ui.button("Add item").clicked() {
            form.lines.push(LineForm::default());
        }
        if ui.button("Save order").clicked() {
            match form
                .draft()
                .and_then(|draft| self.store.create_order(company, &draft))
            {
                Ok(order) => {
                    self.order_screen.form = CreateOrderForm::default();
                    self.order_screen.select(order);
                    self.message = format!("Order {order} saved");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn order_detail_view(&mut self, ui: &mut egui::Ui, company: i64, detail: &OrderDetail) {
        let order = &detail.summary;
        ui.heading(format!(
            "{} order {} | {}",
            order.kind.as_str(),
            order.reference,
            order.status.label()
        ));
        if ui.button("Hide order details").clicked() {
            self.order_screen.selected = None;
            self.order_screen.close_reason.clear();
        }
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
                for label in ["Item", "Ordered", "Invoiced", "Unfulfilled", "Unit price"] {
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
        if order.status.has_pending() {
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
                        self.invoice_order != Some(order.id)
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

    fn prepare_order_invoice(&mut self, detail: &OrderDetail) {
        let order = &detail.summary;
        self.invoice_order = Some(order.id);
        self.invoice_kind = order.kind;
        self.invoice_party = Some(order.party_ledger_id);
        self.invoice_trade = None;
        self.invoice_date.clear();
        self.invoice_reference.clear();
        self.invoice_narration.clear();
        self.invoice_due_date.clear();
        self.invoice_credit_days.clear();
        self.invoice_tax_amount.clear();
        self.invoice_additional_taxes.clear();
        self.clear_invoice_tax_context();
        self.invoice_tax_ledger = None;
        self.invoice_adjustments.clear();
        self.invoice_lines = detail
            .lines
            .iter()
            .filter(|line| line.unfulfilled() > 0)
            .map(|line| {
                let mut form = InvoiceLineForm::new();
                form.item = Some(line.line.item_id);
                form.quantity = format_quantity(line.unfulfilled().into());
                form.price = format_amount(line.line.unit_price);
                form
            })
            .collect();
        self.page = Page::Invoice;
        self.message.clear();
    }
}
