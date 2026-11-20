use super::{money, pick_stock, App};
use balance::actual_cost::{StockAgeingReport, DEFAULT_AGEING_CUTOFFS};
use balance::inventory::format_quantity;
use balance::valuation::{SalesCostReport, StockValuation};
use balance::{format_amount, parse_amount, Error, Result};
use eframe::egui;

#[derive(Default)]
pub(super) struct ValuationScreen {
    item: Option<i64>,
    rate_date: String,
    rate: String,
    reason: String,
    stock_date: String,
    stock: Option<StockValuation>,
    ageing: StockAgeingScreen,
    sales_from: String,
    sales_to: String,
    sales: Option<SalesCostReport>,
}

struct StockAgeingScreen {
    date: String,
    cutoffs: [String; 3],
    report: Option<StockAgeingReport>,
    selected: Option<i64>,
}

impl Default for StockAgeingScreen {
    fn default() -> Self {
        Self {
            date: String::new(),
            cutoffs: DEFAULT_AGEING_CUTOFFS.map(|days| days.to_string()),
            report: None,
            selected: None,
        }
    }
}

impl StockAgeingScreen {
    fn cutoffs(&self) -> Result<[u32; 3]> {
        let parse_days = |value: &str| {
            value.trim().parse::<u32>().map_err(|_| {
                Error::Validation("Enter three increasing age cutoffs in whole days".into())
            })
        };
        let [first, second, third] = &self.cutoffs;
        Ok([parse_days(first)?, parse_days(second)?, parse_days(third)?])
    }
}

impl App {
    pub(super) fn valuation_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Stock valuation");
        ui.small("Valuation reports do not change ledger balances.");
        egui::CollapsingHeader::new("Standard costs")
            .default_open(true)
            .show(ui, |ui| {
                self.cost_rate_form(ui, company);
            });
        ui.separator();
        self.report_export_path_field(ui);
        egui::CollapsingHeader::new("Stock value at actual cost").show(ui, |ui| {
            self.actual_cost_view(ui, company);
        });
        egui::CollapsingHeader::new("Stock value at standard cost").show(ui, |ui| {
            self.standard_stock_view(ui, company);
        });
        egui::CollapsingHeader::new("Stock ageing").show(ui, |ui| {
            if let Err(error) = self.stock_ageing_view(ui, company) {
                self.failure(error);
            }
        });
        egui::CollapsingHeader::new("Item sales and actual margin").show(ui, |ui| {
            self.actual_sales_view(ui, company);
        });
        egui::CollapsingHeader::new("Item sales and standard margin").show(ui, |ui| {
            self.standard_sales_view(ui, company);
        });
    }

    fn stock_ageing_view(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        let form = &mut self.valuation_screen.ageing;
        let (run, export) = ui
            .horizontal_wrapped(|ui| {
                ui.label("As of");
                let mut changed = ui
                    .add(
                        egui::TextEdit::singleline(&mut form.date)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(110.0),
                    )
                    .changed();
                ui.label("Age cutoffs (days)");
                for cutoff in &mut form.cutoffs {
                    changed |= ui
                        .add(egui::TextEdit::singleline(cutoff).desired_width(50.0))
                        .changed();
                }
                if changed {
                    form.report = None;
                    form.selected = None;
                }
                (
                    ui.button("Run stock ageing").clicked(),
                    ui.button("Export ageing CSV").clicked(),
                )
            })
            .inner;
        ui.small("Age uses the latest receipt date of each remaining FIFO quantity across all locations.");
        if run {
            form.report = Some(self.store.stock_ageing(
                company,
                form.date.trim(),
                form.cutoffs()?,
            )?);
            form.selected = None;
            self.message.clear();
        }
        if export {
            self.store.export_stock_ageing_csv(
                company,
                form.date.trim(),
                form.cutoffs()?,
                self.report_export_path.trim(),
            )?;
            self.message = "Stock ageing exported".into();
        }
        let Some(report) = &form.report else {
            return Ok(());
        };
        if report.rows.is_empty() {
            ui.label("No stock to age on this date.");
            return Ok(());
        }
        egui::Grid::new("stock-ageing-summary")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Item");
                ui.strong("Unit");
                ui.strong("On hand");
                for label in report.band_labels() {
                    ui.strong(label);
                }
                ui.strong("FIFO value");
                ui.strong("Report issue");
                ui.end_row();
                for row in &report.rows {
                    if ui
                        .selectable_label(form.selected == Some(row.item.id), &row.item.name)
                        .clicked()
                    {
                        form.selected = Some(row.item.id);
                    }
                    ui.label(&row.item.unit);
                    ui.label(format_quantity(row.quantity));
                    if let Some(breakdown) = &row.breakdown {
                        for band in &breakdown.bands {
                            ui.label(format_quantity(band.quantity));
                        }
                        ui.label(
                            breakdown
                                .fifo_value
                                .map(money)
                                .unwrap_or_else(|| "—".into()),
                        );
                    } else {
                        for _ in 0..5 {
                            ui.label("—");
                        }
                    }
                    ui.label(
                        row.issue
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    );
                    ui.end_row();
                }
            });
        if let Some(row) = report
            .rows
            .iter()
            .find(|row| Some(row.item.id) == form.selected)
        {
            if let Some(breakdown) = &row.breakdown {
                crate::theme::section(ui, format!("Remaining receipts for {}", row.item.name));
                egui::Grid::new(("stock-ageing-receipts", row.item.id))
                    .striped(true)
                    .show(ui, |ui| {
                        for label in ["Receipt date", "Age (days)", "Quantity", "FIFO value"] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for layer in &breakdown.receipts {
                            ui.label(layer.received_on.to_string());
                            ui.label(layer.age_days.to_string());
                            ui.label(format_quantity(layer.amount.quantity));
                            ui.label(
                                layer
                                    .amount
                                    .fifo_value
                                    .map(money)
                                    .unwrap_or_else(|| "—".into()),
                            );
                            ui.end_row();
                        }
                    });
            }
        }
        Ok(())
    }

    fn cost_rate_form(&mut self, ui: &mut egui::Ui, company: i64) {
        let items = match self.store.stock_items(company) {
            Ok(items) => items,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let form = &mut self.valuation_screen;
        ui.horizontal(|ui| pick_stock(ui, "standard-cost-item", "Item", &items, &mut form.item));
        egui::Grid::new("cost-rate-form").show(ui, |ui| {
            for (label, field) in [
                ("Effective date (YYYY-MM-DD)", &mut form.rate_date),
                ("Standard cost per base unit", &mut form.rate),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(field);
                ui.end_row();
            }
        });
        if ui.button("Record standard cost").clicked() {
            let result = parse_amount(&form.rate).and_then(|cost| {
                let item = form
                    .item
                    .ok_or_else(|| Error::Validation("Select a stock item".into()))?;
                self.store.record_standard_stock_cost(
                    company,
                    item,
                    &form.rate_date,
                    cost,
                    &form.reason,
                )
            });
            match result {
                Ok(_) => {
                    form.rate.clear();
                    form.reason.clear();
                    form.stock = None;
                    form.sales = None;
                    self.message = "Standard cost recorded".into();
                }
                Err(error) => self.message = error.to_string(),
            }
        }
        if let Some(item) = form.item {
            egui::CollapsingHeader::new("Rate history").show(ui, |ui| {
                match self.store.standard_stock_cost_history(company, item) {
                    Ok(history) if history.is_empty() => {
                        ui.label("No rates saved for this item.");
                    }
                    Ok(history) => {
                        for rate in history {
                            ui.label(format!(
                                "Effective {} | {} per base unit | {} | {} UTC",
                                rate.date,
                                format_amount(rate.unit_cost),
                                rate.reason,
                                rate.recorded_at
                            ));
                        }
                    }
                    Err(error) => self.failure(error),
                }
            });
        }
    }

    fn standard_stock_view(&mut self, ui: &mut egui::Ui, company: i64) {
        let mut run = false;
        let mut export = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("As of");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.valuation_screen.stock_date)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(110.0),
                )
                .changed()
            {
                self.valuation_screen.stock = None;
            }
            run = ui.button("Run stock valuation").clicked();
            export = ui.button("Export valuation CSV").clicked();
        });
        let date = self.valuation_screen.stock_date.trim();
        if run {
            match self.store.stock_valuation_at_standard_cost(company, date) {
                Ok(report) => {
                    self.valuation_screen.stock = Some(report);
                    self.message.clear();
                }
                Err(error) => {
                    self.valuation_screen.stock = None;
                    self.message = error.to_string();
                }
            }
        }
        if export {
            match self.store.export_stock_valuation_csv(
                company,
                date,
                self.report_export_path.trim(),
            ) {
                Ok(()) => self.message = "Stock valuation exported".into(),
                Err(error) => self.message = error.to_string(),
            }
        }
        let Some(report) = &self.valuation_screen.stock else {
            return;
        };
        ui.label(format!(
            "{} | Known stock value {} | {} items missing a cost",
            report.date,
            money(report.known_value),
            report.missing_rates
        ));
        egui::ScrollArea::both()
            .id_salt("standard-stock-table")
            .max_height(420.0)
            .show(ui, |ui| {
                egui::Grid::new("stock-valuations")
                    .striped(true)
                    .show(ui, |ui| {
                        for title in ["Item", "Quantity", "Standard cost per unit", "Stock value"] {
                            ui.strong(title);
                        }
                        ui.end_row();
                        for row in &report.rows {
                            ui.label(&row.item.name);
                            ui.label(format!(
                                "{} {}",
                                format_quantity(row.quantity),
                                row.item.unit
                            ));
                            optional_cost(ui, row.unit_cost.map(i128::from));
                            optional_cost(ui, row.value);
                            ui.end_row();
                        }
                    });
            });
    }

    fn standard_sales_view(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.small("Stock invoice lines after discounts. A reversal uses its note date and the original sale’s standard rate.");
        let mut run = false;
        let mut export = false;
        ui.horizontal_wrapped(|ui| {
            for (label, input) in [
                ("From", &mut self.valuation_screen.sales_from),
                ("Through", &mut self.valuation_screen.sales_to),
            ] {
                ui.label(label);
                if ui
                    .add(
                        egui::TextEdit::singleline(input)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(110.0),
                    )
                    .changed()
                {
                    self.valuation_screen.sales = None;
                }
            }
            run = ui.button("Run item sales report").clicked();
            export = ui.button("Export item sales CSV").clicked();
        });
        let from = self.valuation_screen.sales_from.trim();
        let to = self.valuation_screen.sales_to.trim();
        let from = (!from.is_empty()).then_some(from);
        let to = (!to.is_empty()).then_some(to);
        if run {
            match self.store.sales_at_standard_cost(company, from, to) {
                Ok(report) => {
                    self.valuation_screen.sales = Some(report);
                    self.message.clear();
                }
                Err(error) => {
                    self.valuation_screen.sales = None;
                    self.message = error.to_string();
                }
            }
        }
        if export {
            match self.store.export_standard_sales_cost_csv(
                company,
                from,
                to,
                self.report_export_path.trim(),
            ) {
                Ok(()) => self.message = "Item sales report exported".into(),
                Err(error) => self.message = error.to_string(),
            }
        }
        let Some(report) = &self.valuation_screen.sales else {
            return;
        };
        ui.label(format!(
            "Net item sales {} | {} lines missing a cost",
            money(report.net_sales),
            report.missing_rates
        ));
        ui.label(format!(
            "Sales with known costs {} | Standard cost {} | Standard margin {}",
            money(report.costed_net_sales),
            money(report.standard_cost),
            money(report.costed_margin)
        ));
        egui::ScrollArea::both()
            .id_salt("standard-sales-table")
            .max_height(420.0)
            .show(ui, |ui| {
                egui::Grid::new("standard-sales-cost")
                    .striped(true)
                    .show(ui, |ui| {
                        for title in [
                            "Date",
                            "Entry",
                            "Reference",
                            "Item",
                            "Quantity",
                            "Net sales",
                            "Standard cost",
                            "Margin",
                        ] {
                            ui.strong(title);
                        }
                        ui.end_row();
                        for row in &report.rows {
                            ui.label(row.date.to_string());
                            ui.label(row.document.label());
                            ui.label(&row.reference);
                            ui.label(&row.item);
                            ui.label(format!("{} {}", format_quantity(row.quantity), row.unit));
                            ui.label(money(row.net_sales));
                            if row.awaiting_stock {
                                ui.label("Awaiting stock issue");
                                ui.label("—");
                            } else {
                                optional_cost(ui, row.standard_cost);
                                optional_cost(ui, row.margin());
                            }
                            ui.end_row();
                        }
                    });
            });
    }
}

fn optional_cost(ui: &mut egui::Ui, amount: Option<i128>) {
    if let Some(amount) = amount {
        ui.label(money(amount));
    } else {
        ui.label("Missing rate");
    }
}
