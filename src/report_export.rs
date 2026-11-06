//! Narrow CSV exports of the reports shown in the application.
use crate::{
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
        contextual_row(
            &mut csv,
            &strings(&["company", "as_of"]),
            &strings(&[
                "row_type",
                "invoice_id",
                "type",
                "party",
                "date",
                "due_date",
                "reference",
                "total",
                "settled",
                "outstanding",
                "age_days",
                "overdue_days",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&["context", "", "", "", "", "", "", "", "", "", "", ""]),
        )?;
        for entry in entries {
            contextual_row(
                &mut csv,
                &context,
                &[
                    "invoice".into(),
                    entry.invoice_id.to_string(),
                    entry.kind.as_str().into(),
                    text_cell(&entry.party),
                    entry.date,
                    entry.due_date.unwrap_or_default(),
                    text_cell(&entry.reference),
                    money(entry.total),
                    money(entry.settled),
                    money(entry.outstanding),
                    entry.age_days.to_string(),
                    entry
                        .overdue_days
                        .map(|d| d.to_string())
                        .unwrap_or_default(),
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
            &strings(&[
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
                "settled_all_dates",
                "outstanding_all_dates",
            ]),
        )?;
        contextual_row(
            &mut csv,
            &context,
            &strings(&[
                "context", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
            ]),
        )?;
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
                    money(invoice.settled),
                    money(invoice.outstanding),
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
