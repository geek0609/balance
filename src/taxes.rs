//! Tax components and reporting for invoices and their reversals.
use crate::inventory::{ledger_category, InvoiceDraft, InvoiceKind};
use crate::{parse_date, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection};
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

#[derive(Debug, Clone)]
pub struct InvoiceTaxLine {
    pub ledger_id: i64,
    pub amount: i64,
}

#[derive(Debug, Clone)]
pub struct InvoiceTaxDetail {
    pub ledger: String,
    pub amount: i64,
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
        conn.execute(
            "INSERT INTO invoice_additional_taxes(invoice_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
            params![invoice, tax.ledger_id, tax.amount],
        )?;
    }
    Ok(())
}

pub(crate) fn additional_lines(conn: &Connection, invoice: i64) -> Result<Vec<InvoiceTaxLine>> {
    let mut stmt = conn.prepare(
        "SELECT ledger_id, amount FROM invoice_additional_taxes WHERE invoice_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([invoice], |row| {
        Ok(InvoiceTaxLine {
            ledger_id: row.get(0)?,
            amount: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn tax_details(
    conn: &Connection,
    id: i64,
    revision: bool,
    primary_name: Option<&str>,
    primary_amount: i64,
) -> Result<Vec<InvoiceTaxDetail>> {
    let sql = if revision {
        "SELECT l.name, t.amount FROM invoice_revision_additional_taxes t JOIN ledgers l ON l.id = t.ledger_id
         WHERE t.revision_id = ?1 ORDER BY t.id"
    } else {
        "SELECT l.name, t.amount FROM invoice_additional_taxes t JOIN ledgers l ON l.id = t.ledger_id
         WHERE t.invoice_id = ?1 ORDER BY t.id"
    };
    let mut taxes = Vec::new();
    if primary_amount > 0 {
        let ledger = primary_name
            .ok_or_else(|| Error::Validation("The original tax ledger is missing".into()))?;
        taxes.push(InvoiceTaxDetail {
            ledger: ledger.into(),
            amount: primary_amount,
        });
    }
    let mut stmt = conn.prepare(sql)?;
    for row in stmt.query_map([id], |row| {
        Ok(InvoiceTaxDetail {
            ledger: row.get(0)?,
            amount: row.get(1)?,
        })
    })? {
        taxes.push(row?);
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
    pub reversal: bool,
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
    /// Report recorded invoice tax, with full reversals on the note date.
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
            "WITH tax(invoice_id, ledger_id, amount) AS (
                SELECT id, tax_ledger_id, tax_amount FROM invoices
                WHERE company_id = ?1 AND tax_amount > 0
                UNION ALL
                SELECT t.invoice_id, t.ledger_id, t.amount FROM invoice_additional_taxes t
                JOIN invoices i ON i.id = t.invoice_id WHERE i.company_id = ?1
             )
             SELECT i.id, i.kind, v.id, v.date, v.reference, p.name, l.id, l.name, tax.amount,
                    rv.id, rv.date, rv.reference
             FROM tax JOIN invoices i ON i.id = tax.invoice_id AND i.company_id = ?1
             JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
             JOIN ledgers p ON p.id = i.party_ledger_id AND p.company_id = i.company_id
             JOIN ledgers l ON l.id = tax.ledger_id AND l.company_id = i.company_id
             LEFT JOIN invoice_reversals r ON r.invoice_id = i.id AND r.company_id = i.company_id
             LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
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
                let amount = if reversal { -amount } else { amount };
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
                    reversal,
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
