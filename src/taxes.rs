//! Tax components and reporting for invoices and their reversals.
use crate::inventory::{ledger_category, InvoiceDraft, InvoiceKind};
use crate::{parse_date, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE invoice_additional_taxes(
            id INTEGER PRIMARY KEY,
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            UNIQUE(invoice_id, ledger_id)
        );
        CREATE TABLE invoice_revision_additional_taxes(
            id INTEGER PRIMARY KEY,
            revision_id INTEGER NOT NULL REFERENCES invoice_revisions(id),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            UNIQUE(revision_id, ledger_id)
        );",
    )?;
    Ok(())
}

pub(crate) fn migrate_source_rates(conn: &Connection) -> Result<()> {
    for table in [
        "invoice_additional_taxes",
        "invoice_revision_additional_taxes",
    ] {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'source_rates')",
            [table],
            |row| row.get(0),
        )?;
        if !present {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN source_rates TEXT NOT NULL DEFAULT ''"),
                [],
            )?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceTaxLine {
    pub ledger_id: i64,
    pub amount: i64,
    /// Percentages recorded on the source posting, in source order.
    pub source_rates: Vec<Decimal>,
}

#[derive(Debug, Clone)]
pub struct InvoiceTaxDetail {
    pub ledger: String,
    pub amount: i64,
    pub source_rates: Vec<Decimal>,
}

fn read_source_rates(value: &str) -> Result<Vec<Decimal>> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|rate| {
            Decimal::from_str_exact(rate)
                .map_err(|_| Error::Validation("Invalid saved source tax rate".into()))
        })
        .collect()
}

pub fn format_source_tax_rates(rates: &[Decimal]) -> String {
    rates
        .iter()
        .map(|rate| format!("{rate}%"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn checked_total(lines: impl Iterator<Item = i64>) -> Result<i64> {
    let total = lines.map(i128::from).sum::<i128>();
    i64::try_from(total).map_err(|_| Error::Validation("Invoice tax total is too large".into()))
}

pub(crate) fn validate_draft_taxes(
    conn: &Connection,
    company: i64,
    draft: &InvoiceDraft,
) -> Result<Vec<InvoiceTaxLine>> {
    if draft.tax_amount < 0 || (draft.tax_amount > 0) != draft.tax_ledger_id.is_some() {
        return Err(Error::Validation(
            "A tax amount requires a ledger and must be positive".into(),
        ));
    }
    let mut rows = Vec::with_capacity(draft.additional_taxes.len() + 1);
    if let Some(ledger_id) = draft.tax_ledger_id {
        rows.push(InvoiceTaxLine {
            ledger_id,
            amount: draft.tax_amount,
            source_rates: Vec::new(),
        });
    }
    rows.extend_from_slice(&draft.additional_taxes);
    let mut ledgers = HashSet::new();
    for row in &rows {
        if row.amount <= 0 || !ledgers.insert(row.ledger_id) {
            return Err(Error::Validation(
                "Enter one positive tax amount per ledger".into(),
            ));
        }
        if row.ledger_id == draft.party_ledger_id || row.ledger_id == draft.trade_ledger_id {
            return Err(Error::Validation(
                "Tax ledgers must differ from the party and trade ledgers".into(),
            ));
        }
        if !matches!(
            ledger_category(conn, company, row.ledger_id)?,
            Category::Asset | Category::Liability
        ) {
            return Err(Error::Validation(
                "A tax ledger must be an asset or liability".into(),
            ));
        }
    }
    Ok(rows)
}

pub(crate) fn write_additional(
    conn: &Connection,
    invoice: i64,
    taxes: &[InvoiceTaxLine],
) -> Result<()> {
    for tax in taxes {
        let rates = tax
            .source_rates
            .iter()
            .map(Decimal::to_string)
            .collect::<Vec<_>>()
            .join(",");
        conn.execute(
            "INSERT INTO invoice_additional_taxes(invoice_id, ledger_id, amount, source_rates)
             VALUES (?1, ?2, ?3, ?4)",
            params![invoice, tax.ledger_id, tax.amount, rates],
        )?;
    }
    Ok(())
}

pub(crate) fn additional_lines(conn: &Connection, invoice: i64) -> Result<Vec<InvoiceTaxLine>> {
    let mut stmt = conn.prepare(
        "SELECT ledger_id, amount, source_rates FROM invoice_additional_taxes
         WHERE invoice_id = ?1 ORDER BY id",
    )?;
    let mut rows = stmt.query([invoice])?;
    let mut taxes = Vec::new();
    while let Some(row) = rows.next()? {
        taxes.push(InvoiceTaxLine {
            ledger_id: row.get(0)?,
            amount: row.get(1)?,
            source_rates: read_source_rates(&row.get::<_, String>(2)?)?,
        });
    }
    Ok(taxes)
}

pub(crate) fn tax_details(
    conn: &Connection,
    id: i64,
    revision: bool,
    primary_name: Option<&str>,
    primary_amount: i64,
) -> Result<Vec<InvoiceTaxDetail>> {
    let sql = if revision {
        "SELECT l.name, t.amount, t.source_rates FROM invoice_revision_additional_taxes t JOIN ledgers l ON l.id = t.ledger_id
         WHERE t.revision_id = ?1 ORDER BY t.id"
    } else {
        "SELECT l.name, t.amount, t.source_rates FROM invoice_additional_taxes t JOIN ledgers l ON l.id = t.ledger_id
         WHERE t.invoice_id = ?1 ORDER BY t.id"
    };
    let mut taxes = Vec::new();
    if primary_amount > 0 {
        let ledger = primary_name
            .ok_or_else(|| Error::Validation("The original tax ledger is missing".into()))?;
        taxes.push(InvoiceTaxDetail {
            ledger: ledger.into(),
            amount: primary_amount,
            source_rates: Vec::new(),
        });
    }
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([id])?;
    while let Some(row) = rows.next()? {
        taxes.push(InvoiceTaxDetail {
            ledger: row.get(0)?,
            amount: row.get(1)?,
            source_rates: read_source_rates(&row.get::<_, String>(2)?)?,
        });
    }
    Ok(taxes)
}

#[derive(Debug, Clone)]
pub struct InvoiceTaxEntry {
    pub date: NaiveDate,
    pub voucher_id: i64,
    pub invoice_id: i64,
    pub kind: InvoiceKind,
    pub reference: String,
    pub party: String,
    pub ledger_id: i64,
    pub ledger: String,
    pub amount: i128,
    pub source_rates: Vec<Decimal>,
    pub reversal: bool,
    pub credit_note: bool,
}

impl InvoiceTaxEntry {
    pub fn document_type(&self) -> &'static str {
        match (self.credit_note, self.reversal, self.kind) {
            (true, true, _) => "Note reversal",
            (true, false, InvoiceKind::Sale) => "Credit note",
            (true, false, InvoiceKind::Purchase) => "Debit note",
            (false, true, _) => "Reversal",
            (false, false, _) => "Invoice",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaxLedgerTotals {
    pub ledger_id: i64,
    pub ledger: String,
    pub sales: i128,
    pub purchases: i128,
}

#[derive(Debug, Clone)]
pub struct InvoiceTaxRegister {
    pub entries: Vec<InvoiceTaxEntry>,
    pub ledgers: Vec<TaxLedgerTotals>,
    pub sales: i128,
    pub purchases: i128,
}

impl Store {
    /// Report recorded invoice tax and subsequent adjustments by their voucher dates.
    pub fn invoice_tax_register(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<InvoiceKind>,
        ledger_id: Option<i64>,
    ) -> Result<InvoiceTaxRegister> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(start), Some(end)) if start > end) {
            return Err(Error::Validation(
                "Tax register start date cannot follow its end date".into(),
            ));
        }
        if let Some(ledger) = ledger_id {
            if !matches!(
                ledger_category(&self.conn, company, ledger)?,
                Category::Asset | Category::Liability
            ) {
                return Err(Error::Validation(
                    "Select an asset or liability tax ledger".into(),
                ));
            }
        }
        let mut stmt = self.conn.prepare(
            "WITH tax(invoice_id, ledger_id, amount, note_id, source_rates) AS (
                SELECT id, tax_ledger_id, tax_amount, NULL, '' FROM invoices
                WHERE company_id = ?1 AND tax_amount > 0
                UNION ALL
                SELECT t.invoice_id, t.ledger_id, t.amount, NULL, t.source_rates FROM invoice_additional_taxes t
                JOIN invoices i ON i.id = t.invoice_id WHERE i.company_id = ?1
                UNION ALL
                SELECT n.invoice_id, l.ledger_id, l.reduction, n.id, '' FROM invoice_credit_notes n
                JOIN invoice_credit_lines l ON l.note_id = n.id AND l.is_tax = 1
                WHERE n.company_id = ?1
             )
             SELECT i.id, i.kind, v.id, v.date, v.reference, p.name, l.id, l.name, tax.amount,
                    rv.id, rv.date, rv.reference, n.id IS NOT NULL, tax.source_rates
             FROM tax JOIN invoices i ON i.id = tax.invoice_id AND i.company_id = ?1
             LEFT JOIN invoice_credit_notes n ON n.id = tax.note_id
             JOIN vouchers v ON v.id = COALESCE(n.voucher_id, i.voucher_id) AND v.company_id = i.company_id
             JOIN ledgers p ON p.id = i.party_ledger_id AND p.company_id = i.company_id
             JOIN ledgers l ON l.id = tax.ledger_id AND l.company_id = i.company_id
             LEFT JOIN invoice_reversals r ON r.invoice_id = i.id AND r.company_id = i.company_id
             LEFT JOIN vouchers rv ON rv.company_id = i.company_id AND rv.id =
                CASE WHEN n.id IS NULL THEN r.reversal_voucher_id ELSE n.reversal_voucher_id END
             WHERE (?2 IS NULL OR i.kind = ?2) AND (?3 IS NULL OR l.id = ?3)",
        )?;
        let mut rows = stmt.query(params![company, kind.map(InvoiceKind::as_str), ledger_id])?;
        let mut entries = Vec::new();
        let mut ledgers = BTreeMap::<i64, TaxLedgerTotals>::new();
        let mut sales = 0_i128;
        let mut purchases = 0_i128;
        while let Some(row) = rows.next()? {
            let kind = InvoiceKind::parse(&row.get::<_, String>(1)?)?;
            let amount = i128::from(row.get::<_, i64>(8)?);
            let credit_note: bool = row.get(12)?;
            let source_rates = read_source_rates(&row.get::<_, String>(13)?)?;
            let ledger_id = row.get(6)?;
            let ledger: String = row.get(7)?;
            let events: [Option<(i64, NaiveDate, String, bool)>; 2] = [
                Some((
                    row.get(2)?,
                    parse_date(&row.get::<_, String>(3)?)?,
                    row.get(4)?,
                    false,
                )),
                match row.get::<_, Option<i64>>(9)? {
                    Some(id) => Some((
                        id,
                        parse_date(&row.get::<_, String>(10)?)?,
                        row.get(11)?,
                        true,
                    )),
                    None => None,
                },
            ];
            for (voucher_id, date, reference, reversal) in events.into_iter().flatten() {
                if start.is_some_and(|cutoff| date < cutoff)
                    || end.is_some_and(|cutoff| date > cutoff)
                {
                    continue;
                }
                let amount = if reversal ^ credit_note {
                    -amount
                } else {
                    amount
                };
                let ledger_total = ledgers.entry(ledger_id).or_insert_with(|| TaxLedgerTotals {
                    ledger_id,
                    ledger: ledger.clone(),
                    ..TaxLedgerTotals::default()
                });
                match kind {
                    InvoiceKind::Sale => {
                        ledger_total.sales += amount;
                        sales += amount;
                    }
                    InvoiceKind::Purchase => {
                        ledger_total.purchases += amount;
                        purchases += amount;
                    }
                }
                entries.push(InvoiceTaxEntry {
                    date,
                    voucher_id,
                    invoice_id: row.get(0)?,
                    kind,
                    reference,
                    party: row.get(5)?,
                    ledger_id,
                    ledger: ledger.clone(),
                    amount,
                    source_rates: source_rates.clone(),
                    reversal,
                    credit_note,
                });
            }
        }
        entries.sort_by_key(|row| (row.date, row.voucher_id, row.ledger_id));
        let mut ledgers = ledgers.into_values().collect::<Vec<_>>();
        ledgers.sort_by(|a, b| {
            a.ledger
                .cmp(&b.ledger)
                .then_with(|| a.ledger_id.cmp(&b.ledger_id))
        });
        Ok(InvoiceTaxRegister {
            entries,
            ledgers,
            sales,
            purchases,
        })
    }
}
