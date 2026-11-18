//! Narrow CSV exports of the reports shown in the application.
use crate::{
    actual_cost::ActualCostMethod,
    inventory::{format_quantity, InvoiceKind},
    parse_date, Error, Result, Store, VoucherKind,
};
use std::{io::Write, path::Path};

fn money(value: i128) -> String {
    let magnitude = value.unsigned_abs();
    format!(
        "{}{}.{:02}",
        if value < 0 { "-" } else { "" },
        magnitude / 100,
        magnitude % 100
    )
}

// Prefix potentially executable spreadsheet cells while retaining the text.
fn text_cell(value: &str) -> String {
    let starts_with_control = value.chars().next().is_some_and(|c| c.is_control());
    let first_visible = value.trim_start_matches(char::is_whitespace).chars().next();
    if starts_with_control || first_visible.is_some_and(|c| matches!(c, '=' | '+' | '-' | '@')) {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

fn writer() -> csv::Writer<Vec<u8>> {
    csv::WriterBuilder::new().from_writer(Vec::new())
}

fn row(writer: &mut csv::Writer<Vec<u8>>, cells: &[String]) -> Result<()> {
    writer
        .write_record(cells)
        .map_err(|e| Error::Validation(format!("Cannot prepare CSV: {e}")))
}

fn contextual_row(
    writer: &mut csv::Writer<Vec<u8>>,
    context: &[String],
    cells: &[String],
) -> Result<()> {
    let mut combined = context.to_vec();
    combined.extend_from_slice(cells);
    row(writer, &combined)
}

fn optional_date(value: Option<&str>) -> Result<String> {
    value.map(parse_date).transpose().map(|date| {
        date.map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    })
}

fn save(writer: csv::Writer<Vec<u8>>, destination: impl AsRef<Path>) -> Result<()> {
    let path = destination.as_ref();
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|v| v.eq_ignore_ascii_case("csv"))
    {
        return Err(Error::Validation(
            "Enter an absolute path to a new .csv file".into(),
        ));
    }
    let bytes = writer
        .into_inner()
        .map_err(|e| Error::Validation(format!("Cannot prepare CSV: {e}")))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::Validation(format!("Cannot create report file: {e}")))?;
    file.write_all(&bytes)
        .and_then(|_| file.flush())
        .map_err(|e| Error::Validation(format!("Cannot finish report file: {e}")))?;
    Ok(())
}

impl Store {
    fn report_company_name(&self, company: i64) -> Result<String> {
        self.companies()?
            .into_iter()
            .find(|c| c.id == company)
            .map(|c| text_cell(&c.name))
            .ok_or_else(|| Error::Validation("Company not found".into()))
    }

    pub fn export_budget_csv(
        &self,
        company: i64,
        version: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.budget_report(company, version)?;
        let context = [
            self.report_company_name(company)?,
            text_cell(&report.version.name),
            report.version.id.to_string(),
            report.version.start_date.clone(),
            report.version.end_date.clone(),
            text_cell(&report.version.scope_label()),
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&[
                "company",
                "budget",
                "version",
                "period_start",
                "period_end",
                "scope",
            ]),
            &strings(&[
                "row_type",
                "ledger_id",
                "category",
                "ledger",
                "planned",
                "actual",
                "favourable_variance",
            ]),
        )?;
        for entry in report.rows {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "ledger".into(),
                    entry.ledger_id.to_string(),
                    entry.category.as_str().into(),
                    text_cell(&entry.ledger),
                    entry
                        .planned
                        .map(|value| money(value.into()))
                        .unwrap_or_default(),
                    money(entry.actual),
                    money(entry.favourable_variance()),
                ],
            )?;
        }
        for (category, planned, actual, variance) in [
            (
                "Income",
                report.planned.income,
                report.actual.income,
                report.actual.income - report.planned.income,
            ),
            (
                "Expense",
                report.planned.expenses,
                report.actual.expenses,
                report.planned.expenses - report.actual.expenses,
            ),
            (
                "Net",
                report.planned.net(),
                report.actual.net(),
                report.actual.net() - report.planned.net(),
            ),
        ] {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "total".into(),
                    String::new(),
                    category.into(),
                    String::new(),
                    money(planned),
                    money(actual),
                    money(variance),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_cost_report_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.cost_report(company, start, end)?;
        let context = vec![
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "period_start", "period_end"]),
            &strings(&[
                "row_type",
                "centre_id",
                "parent_id",
                "centre_path",
                "direct_income",
                "direct_expenses",
                "direct_net",
                "income_with_children",
                "expenses_with_children",
                "net_with_children",
            ]),
        )?;
        for entry in report.rows {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "centre".into(),
                    entry.centre.id.to_string(),
                    entry
                        .centre
                        .parent_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    text_cell(&entry.path),
                    money(entry.direct.income),
                    money(entry.direct.expenses),
                    money(entry.direct.net()),
                    money(entry.including_children.income),
                    money(entry.including_children.expenses),
                    money(entry.including_children.net()),
                ],
            )?;
        }
        for (label, totals) in [
            ("unallocated", report.unallocated),
            ("company_total", report.total),
        ] {
            contextual_row(
                &mut csv,
                &context,
                &[
                    label.into(),
                    String::new(),
                    String::new(),
                    String::new(),
                    money(totals.income),
                    money(totals.expenses),
                    money(totals.net()),
                    String::new(),
                    String::new(),
                    String::new(),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_stock_valuation_csv(
        &self,
        company: i64,
        as_of: &str,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.stock_valuation_at_standard_cost(company, as_of)?;
        let context = [self.report_company_name(company)?, report.date.to_string()];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "item_id",
                "item",
                "unit",
                "quantity",
                "standard_cost_per_unit",
                "standard_stock_value",
                "items_without_cost",
            ]),
        )?;
        for entry in report.rows {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "item".into(),
                    entry.item.id.to_string(),
                    text_cell(&entry.item.name),
                    text_cell(&entry.item.unit),
                    format_quantity(entry.quantity),
                    entry
                        .unit_cost
                        .map(|cost| money(cost.into()))
                        .unwrap_or_default(),
                    entry.value.map(money).unwrap_or_default(),
                    String::new(),
                ],
            )?;
        }
        contextual_row(
            &mut csv,
            &context,
            &[
                "known_value".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                money(report.known_value),
                report.missing_rates.to_string(),
            ],
        )?;
        save(csv, destination)
    }

    pub fn export_average_stock_valuation_csv(
        &self,
        company: i64,
        as_of: &str,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        self.export_actual_stock_valuation_csv(
            company,
            as_of,
            ActualCostMethod::WeightedAverage,
            destination,
        )
    }

    pub fn export_actual_stock_valuation_csv(
        &self,
        company: i64,
        as_of: &str,
        method: ActualCostMethod,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.stock_valuation_at_actual_cost(company, as_of, method)?;
        let value_column = match method {
            ActualCostMethod::WeightedAverage => "weighted_average_stock_value",
            ActualCostMethod::Fifo => "fifo_stock_value",
        };
        let context = [self.report_company_name(company)?, report.date.to_string()];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "item_id",
                "item",
                "unit",
                "quantity",
                value_column,
                "issue",
                "items_unavailable",
            ]),
        )?;
        for entry in report.rows {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "item".into(),
                    entry.item.id.to_string(),
                    text_cell(&entry.item.name),
                    text_cell(&entry.item.unit),
                    format_quantity(entry.quantity),
                    entry.value.map(money).unwrap_or_default(),
                    entry
                        .issue
                        .map(|issue| text_cell(&issue.to_string()))
                        .unwrap_or_default(),
                    String::new(),
                ],
            )?;
        }
        contextual_row(
            &mut csv,
            &context,
            &[
                "known_value".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                money(report.known_value),
                String::new(),
                report.unavailable.to_string(),
            ],
        )?;
        save(csv, destination)
    }

    pub fn export_actual_sales_cost_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        method: ActualCostMethod,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.sales_at_actual_cost(company, start, end, method)?;
        let context = [
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
            report.method.label().into(),
        ];
        let mut csv = writer();
        let details = strings(&[
            "row_type",
            "invoice_id",
            "voucher_id",
            "line_id",
            "date",
            "reference",
            "item_id",
            "item",
            "unit",
            "quantity",
            "net_item_sales",
            "cost_of_sales",
            "item_margin",
            "cost_issue",
            "lines_without_cost",
        ]);
        contextual_row(
            &mut csv,
            &strings(&["company", "period_start", "period_end", "cost_method"]),
            &details,
        )?;
        for row in report.rows {
            let sale = &row.sale;
            contextual_row(
                &mut csv,
                &context,
                &[
                    sale.document.label().to_ascii_lowercase().replace(' ', "_"),
                    sale.invoice_id.to_string(),
                    sale.voucher_id.to_string(),
                    sale.line_id.to_string(),
                    sale.date.to_string(),
                    text_cell(&sale.reference),
                    sale.item_id.to_string(),
                    text_cell(&sale.item),
                    text_cell(&sale.unit),
                    format_quantity(sale.quantity),
                    money(sale.net_sales),
                    row.cost.map(money).unwrap_or_default(),
                    row.margin().map(money).unwrap_or_default(),
                    row.issue
                        .map(|issue| text_cell(&issue.to_string()))
                        .unwrap_or_default(),
                    String::new(),
                ],
            )?;
        }
        let mut summary = vec![String::new(); details.len()];
        summary[0] = "all_sales".into();
        summary[10] = money(report.net_sales);
        summary[14] = report.unavailable_lines.to_string();
        contextual_row(&mut csv, &context, &summary)?;
        summary[0] = "costed_sales".into();
        summary[10] = money(report.costed_net_sales);
        summary[11] = money(report.cost_of_sales);
        summary[12] = money(report.costed_margin);
        summary[14].clear();
        contextual_row(&mut csv, &context, &summary)?;
        save(csv, destination)
    }

    pub fn export_standard_sales_cost_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.sales_at_standard_cost(company, start, end)?;
        let context = [
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "period_start", "period_end"]),
            &strings(&[
                "row_type",
                "invoice_id",
                "voucher_id",
                "date",
                "reference",
                "item",
                "unit",
                "quantity",
                "net_item_sales",
                "standard_cost_per_unit",
                "standard_cost",
                "standard_margin",
                "lines_without_cost",
            ]),
        )?;
        for entry in report.rows {
            let margin = entry.margin().map(money).unwrap_or_default();
            contextual_row(
                &mut csv,
                &context,
                &[
                    entry
                        .document
                        .label()
                        .to_ascii_lowercase()
                        .replace(' ', "_"),
                    entry.invoice_id.to_string(),
                    entry.voucher_id.to_string(),
                    entry.date.to_string(),
                    text_cell(&entry.reference),
                    text_cell(&entry.item),
                    text_cell(&entry.unit),
                    format_quantity(entry.quantity),
                    money(entry.net_sales),
                    entry
                        .unit_cost
                        .map(|cost| money(cost.into()))
                        .unwrap_or_default(),
                    entry.standard_cost.map(money).unwrap_or_default(),
                    margin,
                    String::new(),
                ],
            )?;
        }
        let mut summary = vec![String::new(); 13];
        summary[0] = "all_sales".into();
        summary[8] = money(report.net_sales);
        summary[12] = report.missing_rates.to_string();
        contextual_row(&mut csv, &context, &summary)?;
        summary[0] = "costed_sales".into();
        summary[8] = money(report.costed_net_sales);
        summary[10] = money(report.standard_cost);
        summary[11] = money(report.costed_margin);
        summary[12].clear();
        contextual_row(&mut csv, &context, &summary)?;
        save(csv, destination)
    }

    pub fn export_invoice_tax_register_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<InvoiceKind>,
        ledger: Option<i64>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.invoice_tax_register(company, start, end, kind, ledger)?;
        let context = [
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
            kind.map(InvoiceKind::as_str).unwrap_or("All").into(),
            ledger.map(|id| id.to_string()).unwrap_or_default(),
        ];
        let mut csv = writer();
        let columns = strings(&[
            "row_type",
            "date",
            "voucher_id",
            "invoice_id",
            "invoice_type",
            "reference",
            "party",
            "ledger_id",
            "tax_ledger",
            "amount",
            "sales_tax",
            "purchase_tax",
        ]);
        contextual_row(
            &mut csv,
            &strings(&[
                "company",
                "period_start",
                "period_end",
                "invoice_type_filter",
                "ledger_filter_id",
            ]),
            &columns,
        )?;
        for row in report.entries {
            contextual_row(
                &mut csv,
                &context,
                &[
                    row.document_type().to_ascii_lowercase().replace(' ', "_"),
                    row.date.to_string(),
                    row.voucher_id.to_string(),
                    row.invoice_id.to_string(),
                    row.kind.as_str().into(),
                    text_cell(&row.reference),
                    text_cell(&row.party),
                    row.ledger_id.to_string(),
                    text_cell(&row.ledger),
                    money(row.amount),
                    String::new(),
                    String::new(),
                ],
            )?;
        }
        for row in report.ledgers {
            let mut totals = vec![String::new(); columns.len()];
            totals[0] = "tax_ledger_total".into();
            totals[7] = row.ledger_id.to_string();
            totals[8] = text_cell(&row.ledger);
            totals[10] = money(row.sales);
            totals[11] = money(row.purchases);
            contextual_row(&mut csv, &context, &totals)?;
        }
        let mut totals = vec![String::new(); columns.len()];
        totals[0] = "total".into();
        totals[10] = money(report.sales);
        totals[11] = money(report.purchases);
        contextual_row(&mut csv, &context, &totals)?;
        save(csv, destination)
    }

    pub fn export_payroll_register_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        employee: Option<i64>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let entries = self.payroll_register(company, start, end, employee)?;
        let context = [
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
            employee.map(|id| id.to_string()).unwrap_or_default(),
        ];
        let mut csv = writer();
        let details = strings(&[
            "row_type",
            "accrual_id",
            "voucher_id",
            "date",
            "employee_id",
            "employee_code",
            "employee",
            "department",
            "period_start",
            "period_end",
            "reference",
            "gross",
            "deductions",
            "net",
            "employer_contributions",
            "paid",
            "outstanding",
            "reversal_date",
            "reversal_voucher_id",
        ]);
        contextual_row(
            &mut csv,
            &strings(&["company", "date_from", "date_through", "employee_filter_id"]),
            &details,
        )?;
        let mut totals = [0_i128; 6];
        for row in entries {
            if row.reversal.is_none() {
                for (total, value) in totals.iter_mut().zip([
                    i128::from(row.gross),
                    i128::from(row.deductions),
                    i128::from(row.net),
                    i128::from(row.employer_contributions),
                    row.paid,
                    row.outstanding,
                ]) {
                    *total += value;
                }
            }
            contextual_row(
                &mut csv,
                &context,
                &[
                    "accrual".into(),
                    row.id.to_string(),
                    row.voucher_id.to_string(),
                    row.date,
                    row.employee_id.to_string(),
                    text_cell(&row.code),
                    text_cell(&row.employee),
                    text_cell(&row.department),
                    row.period_start,
                    row.period_end,
                    text_cell(&row.reference),
                    money(row.gross.into()),
                    money(row.deductions.into()),
                    money(row.net.into()),
                    money(row.employer_contributions.into()),
                    money(row.paid),
                    money(row.outstanding),
                    row.reversal
                        .as_ref()
                        .map(|r| r.date.clone())
                        .unwrap_or_default(),
                    row.reversal
                        .as_ref()
                        .map(|r| r.voucher_id.to_string())
                        .unwrap_or_default(),
                ],
            )?;
        }
        let mut summary = vec![String::new(); details.len()];
        summary[0] = "active_total".into();
        for (index, total) in totals.into_iter().enumerate() {
            summary[11 + index] = money(total);
        }
        contextual_row(&mut csv, &context, &summary)?;
        save(csv, destination)
    }

    pub fn export_cash_flow_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.cash_flow_report(company, start, end)?;
        let context = [
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
        ];
        let mut csv = writer();
        let details = strings(&[
            "row_type",
            "date",
            "voucher_id",
            "voucher_type",
            "reference",
            "classification",
            "counterpart",
            "reason",
            "inflows",
            "outflows",
            "net",
            "balance",
        ]);
        contextual_row(
            &mut csv,
            &strings(&["company", "period_start", "period_end"]),
            &details,
        )?;
        let mut opening = vec![String::new(); details.len()];
        opening[0] = "opening".into();
        opening[11] = money(report.opening);
        contextual_row(&mut csv, &context, &opening)?;
        for entry in report.entries {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "voucher".into(),
                    entry.date,
                    entry.voucher_id.to_string(),
                    entry.kind.as_str().into(),
                    text_cell(&entry.reference),
                    entry
                        .class
                        .map(|class| class.label())
                        .unwrap_or("Net zero")
                        .into(),
                    entry
                        .counterpart
                        .map(|name| text_cell(&name))
                        .unwrap_or_default(),
                    entry.explanation.into(),
                    money(entry.amount.max(0)),
                    money((-entry.amount).max(0)),
                    money(entry.amount),
                    String::new(),
                ],
            )?;
        }
        for (class, amount) in report.totals {
            let mut total = vec![String::new(); details.len()];
            total[0] = "classification_total".into();
            total[5] = class.label().into();
            total[8] = money(amount.inflows);
            total[9] = money(amount.outflows);
            total[10] = money(amount.net());
            contextual_row(&mut csv, &context, &total)?;
        }
        opening[0] = "closing".into();
        opening[11] = money(report.closing);
        contextual_row(&mut csv, &context, &opening)?;
        save(csv, destination)
    }

    pub fn export_day_book_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<VoucherKind>,
        search: &str,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let entries = self.day_book(company, start, end, kind, search)?;
        let context = vec![
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
            kind.map(|k| k.as_str()).unwrap_or("All").into(),
            text_cell(search.trim()),
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&[
                "company",
                "period_start",
                "period_end",
                "type_filter",
                "search",
            ]),
            &strings(&[
                "row_type",
                "date",
                "voucher_id",
                "type",
                "reference",
                "narration",
                "debit",
                "credit",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", ""]),
        )?;
        for entry in entries {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "voucher".into(),
                    entry.voucher.date,
                    entry.voucher.id.to_string(),
                    entry.voucher.kind.as_str().to_owned(),
                    text_cell(&entry.voucher.reference),
                    text_cell(&entry.voucher.narration),
                    money(entry.debit_total),
                    money(entry.credit_total),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_ledger_register_csv(
        &self,
        company: i64,
        ledger: i64,
        start: Option<&str>,
        end: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let register = self.ledger_register(company, ledger, start, end)?;
        let name = text_cell(&register.ledger.name);
        let context = vec![
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "period_start", "period_end"]),
            &strings(&[
                "ledger",
                "row_type",
                "date",
                "voucher_id",
                "narration",
                "debit",
                "credit",
                "balance",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &[
                name.clone(),
                "opening".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                money(register.opening_balance),
            ],
        )?;
        for entry in register.entries {
            let amount = i128::from(entry.amount);
            contextual_row(
                &mut csv,
                &context,
                &[
                    name.clone(),
                    "posting".into(),
                    entry.date,
                    entry.voucher_id.to_string(),
                    text_cell(&entry.narration),
                    if amount > 0 {
                        money(amount)
                    } else {
                        String::new()
                    },
                    if amount < 0 {
                        money(-amount)
                    } else {
                        String::new()
                    },
                    money(entry.running_balance),
                ],
            )?;
        }
        contextual_row(
            &mut csv,
            &context,
            &[
                name,
                "closing".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                money(register.closing_balance),
            ],
        )?;
        save(csv, destination)
    }

    pub fn export_refund_balances_csv(
        &self,
        company: i64,
        as_of: &str,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let date = parse_date(as_of)?.to_string();
        let entries = self.invoice_refund_balances(company, &date)?;
        let context = vec![self.report_company_name(company)?, date];
        let headings = strings(&[
            "row_type",
            "party",
            "invoice_id",
            "invoice_reference",
            "note_id",
            "note_reference",
            "note_date",
            "direction",
            "issued_credit",
            "outstanding_refund",
        ]);
        let mut csv = writer();
        contextual_row(&mut csv, &strings(&["company", "as_of"]), &headings)?;
        let mut empty = vec![String::new(); headings.len()];
        empty[0] = "context".into();
        contextual_row(&mut csv, &context, &empty)?;
        for row in entries {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "refund_balance".into(),
                    text_cell(&row.party),
                    row.invoice_id.to_string(),
                    text_cell(&row.invoice_reference),
                    row.note_id.to_string(),
                    text_cell(&row.note_reference),
                    row.note_date,
                    match row.kind {
                        InvoiceKind::Sale => "pay",
                        InvoiceKind::Purchase => "receive",
                    }
                    .into(),
                    money(row.credit.into()),
                    money(row.outstanding.into()),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_outstanding_csv(
        &self,
        company: i64,
        as_of: &str,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let date = parse_date(as_of)?.format("%Y-%m-%d").to_string();
        let entries = self.outstanding_invoices(company, &date)?;
        let context = vec![self.report_company_name(company)?, date];
        let mut csv = writer();
        let details = strings(&[
            "row_type",
            "invoice_id",
            "type",
            "party",
            "date",
            "due_date",
            "reference",
            "total",
            "settled",
            "advance_applied",
            "outstanding",
            "age_days",
            "overdue_days",
            "credited",
        ]);
        contextual_row(&mut csv, &strings(&["company", "as_of"]), &details)?;
        let mut context_fields = vec![String::new(); details.len()];
        context_fields[0] = "context".into();
        contextual_row(&mut csv, &context, &context_fields)?;
        for entry in entries {
            contextual_row(
                &mut csv,
                &context,
                &[
                    if entry.opening_date.is_some() {
                        "opening_bill"
                    } else {
                        "invoice"
                    }
                    .into(),
                    entry.invoice_id.to_string(),
                    entry.kind.as_str().into(),
                    text_cell(&entry.party),
                    entry.date,
                    entry.due_date.unwrap_or_default(),
                    text_cell(&entry.reference),
                    money(entry.total),
                    money(entry.settled),
                    money(entry.advance_applied),
                    money(entry.outstanding),
                    entry.age_days.to_string(),
                    entry
                        .overdue_days
                        .map(|d| d.to_string())
                        .unwrap_or_default(),
                    money(entry.credited),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_invoice_register_csv(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<InvoiceKind>,
        party: Option<i64>,
        search: &str,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let invoices = self.invoice_register(company, start, end, kind, party, search)?;
        let context = vec![
            self.report_company_name(company)?,
            optional_date(start)?,
            optional_date(end)?,
            kind.map(|value| value.as_str()).unwrap_or("All").into(),
            party.map(|value| value.to_string()).unwrap_or_default(),
            text_cell(search.trim()),
        ];
        let details = strings(&[
            "row_type",
            "invoice_id",
            "voucher_id",
            "date",
            "type",
            "party_id",
            "party",
            "reference",
            "due_date",
            "stock_net",
            "manual_tax",
            "service_and_adjustments",
            "total",
            "reversal_voucher_id",
            "reversal_date",
            "reversal_reference",
            "settled_all_dates",
            "advance_applied_all_dates",
            "outstanding_all_dates",
            "credited_all_dates",
        ]);
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&[
                "company",
                "period_start",
                "period_end",
                "type_filter",
                "party_filter_id",
                "search",
            ]),
            &details,
        )?;
        let mut context_fields = vec![String::new(); details.len()];
        context_fields[0] = "context".into();
        contextual_row(&mut csv, &context, &context_fields)?;
        for invoice in invoices {
            let stock: i128 = invoice
                .lines
                .iter()
                .map(|line| i128::from(line.amount))
                .sum();
            let adjustments: i128 = invoice
                .adjustments
                .iter()
                .map(|line| i128::from(line.amount))
                .sum();
            contextual_row(
                &mut csv,
                &context,
                &[
                    "invoice".into(),
                    invoice.id.to_string(),
                    invoice.voucher_id.to_string(),
                    invoice.date,
                    invoice.kind.as_str().into(),
                    invoice.party_ledger_id.to_string(),
                    text_cell(&invoice.party),
                    text_cell(&invoice.reference),
                    invoice.due_date.unwrap_or_default(),
                    money(stock),
                    money(i128::from(invoice.tax_amount)),
                    money(adjustments),
                    money(invoice.total),
                    invoice
                        .reversal
                        .as_ref()
                        .map(|r| r.voucher_id.to_string())
                        .unwrap_or_default(),
                    invoice
                        .reversal
                        .as_ref()
                        .map(|r| r.date.clone())
                        .unwrap_or_default(),
                    invoice
                        .reversal
                        .as_ref()
                        .map(|r| text_cell(&r.reference))
                        .unwrap_or_default(),
                    money(invoice.settled),
                    money(invoice.advance_applied),
                    money(invoice.outstanding),
                    money(invoice.credited),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_replenishment_csv(
        &self,
        company: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let context = [self.report_company_name(company)?, "all saved dates".into()];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "date_scope"]),
            &strings(&[
                "row_type",
                "item_id",
                "item",
                "unit",
                "on_hand",
                "open_sales",
                "open_purchases",
                "projected",
                "reorder_level",
                "reorder_lot",
                "suggested_quantity",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", "", "", "", ""]),
        )?;
        for row in self.replenishment_suggestions(company)? {
            let stock = row.stock;
            contextual_row(
                &mut csv,
                &context,
                &[
                    "suggestion".into(),
                    stock.item.id.to_string(),
                    text_cell(&stock.item.name),
                    text_cell(&stock.item.unit),
                    format_quantity(stock.on_hand),
                    format_quantity(stock.open_sales),
                    format_quantity(stock.open_purchases),
                    format_quantity(stock.projected()),
                    format_quantity(row.rule.level.into()),
                    format_quantity(row.rule.quantity.into()),
                    format_quantity(row.suggested_quantity),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_order_availability_csv(
        &self,
        company: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let context = [self.report_company_name(company)?, "all saved dates".into()];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "date_scope"]),
            &strings(&[
                "row_type",
                "item_id",
                "item",
                "unit",
                "on_hand",
                "open_sales",
                "open_purchases",
                "after_sales",
                "projected",
                "reserved",
                "unreserved",
                "in_transit",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", "", "", "", "", ""]),
        )?;
        for availability in self.order_availability(company)? {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "item".into(),
                    availability.item.id.to_string(),
                    text_cell(&availability.item.name),
                    text_cell(&availability.item.unit),
                    format_quantity(availability.on_hand),
                    format_quantity(availability.open_sales),
                    format_quantity(availability.open_purchases),
                    format_quantity(availability.after_sales()),
                    format_quantity(availability.projected()),
                    format_quantity(availability.reserved),
                    format_quantity(availability.unreserved()),
                    format_quantity(availability.in_transit),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_pending_order_schedule_csv(
        &self,
        company: i64,
        kind: Option<InvoiceKind>,
        expected_through: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let context = [
            self.report_company_name(company)?,
            kind.map(InvoiceKind::as_str).unwrap_or("all").into(),
            optional_date(expected_through)?,
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "kind_filter", "expected_through"]),
            &strings(&[
                "row_type",
                "order_id",
                "kind",
                "reference",
                "party",
                "expected_on",
                "item_id",
                "item",
                "unit",
                "remaining",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", "", "", ""]),
        )?;
        for line in self.pending_order_schedule(company, kind, expected_through)? {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "order_line".into(),
                    line.order_id.to_string(),
                    line.kind.as_str().into(),
                    text_cell(&line.reference),
                    text_cell(&line.party),
                    line.expected_on
                        .map(|date| date.to_string())
                        .unwrap_or_default(),
                    line.item_id.to_string(),
                    text_cell(&line.item),
                    text_cell(&line.unit),
                    format_quantity(i128::from(line.remaining)),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_receipt_reconciliation_csv(
        &self,
        company: i64,
        as_of: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let context = [self.report_company_name(company)?, optional_date(as_of)?];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "receipt_id",
                "receipt_date",
                "reference",
                "supplier",
                "item",
                "location",
                "batch",
                "unit",
                "received_quantity",
                "invoiced_quantity",
                "unbilled_quantity",
                "receipt_acquisition_cost",
                "cost_for_invoiced_quantity",
                "invoice_net",
                "price_variance",
                "returned_quantity",
            ]),
        )?;
        let mut empty = vec![String::new(); 17];
        empty[0] = "context".into();
        contextual_row(&mut csv, &context, &empty)?;
        for line in self.receipt_reconciliation(company, as_of)? {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "receipt_line".into(),
                    line.note_id.to_string(),
                    text_cell(&line.note_date.to_string()),
                    text_cell(&line.reference),
                    text_cell(&line.supplier),
                    text_cell(&line.item),
                    text_cell(&line.location),
                    text_cell(line.batch.as_deref().unwrap_or_default()),
                    text_cell(&line.unit),
                    format_quantity(i128::from(line.received_quantity)),
                    format_quantity(i128::from(line.invoiced_quantity)),
                    format_quantity(i128::from(line.unbilled_quantity())),
                    line.receipt_cost.map(money).unwrap_or_default(),
                    line.invoiced_receipt_cost.map(money).unwrap_or_default(),
                    money(line.invoice_net),
                    line.variance().map(money).unwrap_or_default(),
                    format_quantity(i128::from(line.returned_quantity)),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_stock_ageing_csv(
        &self,
        company: i64,
        as_of: &str,
        cutoffs: [u32; 3],
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.stock_ageing(company, as_of, cutoffs)?;
        let context = [
            self.report_company_name(company)?,
            report.date.to_string(),
            cutoffs[0].to_string(),
            cutoffs[1].to_string(),
            cutoffs[2].to_string(),
        ];
        let columns = [
            "row_type",
            "item_id",
            "item",
            "unit",
            "quantity",
            "fifo_value",
            "receipt_date",
            "age_days",
            "band_1_quantity",
            "band_2_quantity",
            "band_3_quantity",
            "band_4_quantity",
            "band_1_value",
            "band_2_value",
            "band_3_value",
            "band_4_value",
            "issue",
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&[
                "company",
                "as_of",
                "through_days_1",
                "through_days_2",
                "through_days_3",
            ]),
            &strings(&columns),
        )?;
        let mut metadata = vec![String::new(); columns.len()];
        metadata[0] = "context".into();
        contextual_row(&mut csv, &context, &metadata)?;
        for row in report.rows {
            let row_fields = |kind: &str, quantity: i128, value: Option<i128>| {
                vec![
                    kind.into(),
                    row.item.id.to_string(),
                    text_cell(&row.item.name),
                    text_cell(&row.item.unit),
                    format_quantity(quantity),
                    value.map(money).unwrap_or_default(),
                ]
            };
            let mut line = row_fields(
                "item",
                row.quantity,
                row.breakdown.as_ref().and_then(|aged| aged.fifo_value),
            );
            line.extend([String::new(), String::new()]);
            if let Some(breakdown) = &row.breakdown {
                line.extend(breakdown.bands.map(|band| format_quantity(band.quantity)));
                line.extend(
                    breakdown
                        .bands
                        .map(|band| band.fifo_value.map(money).unwrap_or_default()),
                );
            }
            line.resize(columns.len() - 1, String::new());
            line.push(
                row.issue
                    .as_ref()
                    .map(|issue| text_cell(&issue.to_string()))
                    .unwrap_or_default(),
            );
            contextual_row(&mut csv, &context, &line)?;
            if let Some(breakdown) = row.breakdown {
                for receipt in breakdown.receipts {
                    let mut line = row_fields(
                        "receipt",
                        receipt.amount.quantity,
                        receipt.amount.fifo_value,
                    );
                    line.extend([
                        receipt.received_on.to_string(),
                        receipt.age_days.to_string(),
                    ]);
                    line.resize(columns.len(), String::new());
                    contextual_row(&mut csv, &context, &line)?;
                }
            }
        }
        save(csv, destination)
    }

    pub fn export_stock_dispatches_csv(
        &self,
        company: i64,
        as_of: Option<&str>,
        include_completed: bool,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let context = [
            self.report_company_name(company)?,
            optional_date(as_of)?,
            if include_completed {
                "all dispatches"
            } else {
                "in transit"
            }
            .into(),
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of", "dispatch_scope"]),
            &strings(&[
                "row_type",
                "reference",
                "dispatch_date",
                "item",
                "unit",
                "batch",
                "source",
                "destination",
                "dispatched",
                "received",
                "returned",
                "in_transit",
                "reason",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", "", "", "", "", "", ""]),
        )?;
        for row in self.stock_dispatches(company, as_of)? {
            if !include_completed && row.in_transit() == 0 {
                continue;
            }
            contextual_row(
                &mut csv,
                &context,
                &[
                    "dispatch".into(),
                    text_cell(&row.reference),
                    row.date.clone(),
                    text_cell(&row.item),
                    text_cell(&row.unit),
                    text_cell(row.batch.as_deref().unwrap_or_default()),
                    text_cell(&row.from),
                    text_cell(&row.to),
                    format_quantity(row.quantity.into()),
                    format_quantity(row.received.into()),
                    format_quantity(row.returned.into()),
                    format_quantity(row.in_transit().into()),
                    text_cell(&row.reason),
                ],
            )?;
        }
        save(csv, destination)
    }

    /// Export direct or rolled-up quantities for a selected location.
    pub fn export_stock_location_csv(
        &self,
        company: i64,
        location_id: i64,
        include_descendants: bool,
        as_of: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let location = self
            .stock_locations(company)?
            .into_iter()
            .find(|location| location.id == location_id)
            .ok_or_else(|| Error::Validation("Stock location not found".into()))?;
        let balances =
            self.location_stock_quantities(company, location_id, include_descendants, as_of)?;
        let scope = if include_descendants {
            "location and descendants"
        } else {
            "direct location"
        };
        let context = [
            self.report_company_name(company)?,
            location.id.to_string(),
            text_cell(&location.name),
            scope.into(),
            optional_date(as_of)?,
        ];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "location_id", "location", "scope", "as_of"]),
            &strings(&["row_type", "item_id", "item", "unit", "on_hand"]),
        )?;
        contextual_row(&mut csv, &context, &strings(&["context", "", "", "", ""]))?;
        for balance in balances {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "item".into(),
                    balance.item.id.to_string(),
                    text_cell(&balance.item.name),
                    text_cell(&balance.item.unit),
                    format_quantity(balance.quantity),
                ],
            )?;
        }
        save(csv, destination)
    }

    /// Export company-wide quantities and current reorder settings.
    pub fn export_stock_csv(
        &self,
        company: i64,
        as_of: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let balances = self.stock_balances(company, as_of)?;
        let context = vec![self.report_company_name(company)?, optional_date(as_of)?];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "item_id",
                "item",
                "unit",
                "on_hand",
                "reorder_enabled",
                "reorder_level",
                "suggested_quantity",
                "low_stock",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", "", ""]),
        )?;
        for balance in balances {
            let rule = self.reorder_rule(company, balance.item.id)?;
            let enabled = rule
                .as_ref()
                .map(|r| if r.enabled { "yes" } else { "no" })
                .unwrap_or("");
            let level = rule
                .as_ref()
                .map(|r| format_quantity(i128::from(r.level)))
                .unwrap_or_default();
            let quantity = rule
                .as_ref()
                .map(|r| format_quantity(i128::from(r.quantity)))
                .unwrap_or_default();
            let low = rule
                .as_ref()
                .map(|r| {
                    if r.enabled && balance.quantity <= i128::from(r.level) {
                        "yes"
                    } else {
                        "no"
                    }
                })
                .unwrap_or("");
            contextual_row(
                &mut csv,
                &context,
                &[
                    "item".into(),
                    balance.item.id.to_string(),
                    text_cell(&balance.item.name),
                    text_cell(&balance.item.unit),
                    format_quantity(balance.quantity),
                    enabled.into(),
                    level,
                    quantity,
                    low.into(),
                ],
            )?;
        }
        save(csv, destination)
    }

    pub fn export_group_trial_balance_csv(
        &self,
        company: i64,
        end: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let report = self.grouped_trial_balance(company, end)?;
        let context = vec![self.report_company_name(company)?, optional_date(end)?];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "group_id",
                "parent_id",
                "group_path",
                "category",
                "ledger_id",
                "ledger_name",
                "direct_debit",
                "direct_credit",
                "including_descendants_debit",
                "including_descendants_credit",
            ]),
        )?;
        let mut heading = vec!["context".into()];
        heading.resize(11, String::new());
        contextual_row(&mut csv, &context, &heading)?;
        for group in report.groups {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "group".into(),
                    group.group.id.to_string(),
                    group
                        .group
                        .parent_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    text_cell(&group.path.join(" / ")),
                    group.group.category.as_str().into(),
                    String::new(),
                    String::new(),
                    money(group.direct_debit),
                    money(group.direct_credit),
                    money(group.total_debit),
                    money(group.total_credit),
                ],
            )?;
        }
        for balance in report.ungrouped {
            let amount = i128::from(balance.amount);
            contextual_row(
                &mut csv,
                &context,
                &[
                    "ungrouped_ledger".into(),
                    String::new(),
                    String::new(),
                    String::new(),
                    balance.ledger.category.as_str().into(),
                    balance.ledger.id.to_string(),
                    text_cell(&balance.ledger.name),
                    money(amount.max(0)),
                    money((-amount).max(0)),
                    String::new(),
                    String::new(),
                ],
            )?;
        }
        contextual_row(
            &mut csv,
            &context,
            &[
                "grand_total".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                money(report.debit),
                money(report.credit),
            ],
        )?;
        save(csv, destination)
    }

    pub fn export_trial_balance_csv(
        &self,
        company: i64,
        end: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let balances = self.balances_in_period(company, None, end)?;
        let context = vec![self.report_company_name(company)?, optional_date(end)?];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "ledger_id",
                "ledger",
                "category",
                "group_id",
                "debit",
                "credit",
                "balance",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", ""]),
        )?;
        let (mut debit, mut credit) = (0_i128, 0_i128);
        for balance in balances {
            let amount = i128::from(balance.amount);
            debit += amount.max(0);
            credit += (-amount).max(0);
            contextual_row(
                &mut csv,
                &context,
                &[
                    "ledger".into(),
                    balance.ledger.id.to_string(),
                    text_cell(&balance.ledger.name),
                    balance.ledger.category.as_str().into(),
                    balance
                        .ledger
                        .group_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    money(amount.max(0)),
                    money((-amount).max(0)),
                    money(amount),
                ],
            )?;
        }
        contextual_row(
            &mut csv,
            &context,
            &[
                "grand_total".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                money(debit),
                money(credit),
                money(debit - credit),
            ],
        )?;
        save(csv, destination)
    }

    pub fn export_party_outstanding_csv(
        &self,
        company: i64,
        as_of: &str,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let date = parse_date(as_of)?.format("%Y-%m-%d").to_string();
        let rows = self.party_outstanding(company, &date)?;
        let context = vec![self.report_company_name(company)?, date];
        let mut csv = writer();
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "party_id",
                "party",
                "open_invoices",
                "receivable",
                "payable",
                "overdue_receivable",
                "overdue_payable",
                "current_informational_credit_limit",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", "", ""]),
        )?;
        for entry in rows {
            let terms = self.ledger_terms(company, entry.party_ledger_id)?;
            contextual_row(
                &mut csv,
                &context,
                &[
                    "party".into(),
                    entry.party_ledger_id.to_string(),
                    text_cell(&entry.party),
                    entry.invoices.to_string(),
                    money(entry.receivable),
                    money(entry.payable),
                    money(entry.overdue_receivable),
                    money(entry.overdue_payable),
                    terms
                        .credit_limit
                        .map(|value| money(i128::from(value)))
                        .unwrap_or_default(),
                ],
            )?;
        }
        save(csv, destination)
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
