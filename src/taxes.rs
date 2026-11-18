//! Manual tax components on invoices.
use crate::inventory::{ledger_category, InvoiceDraft};
use crate::{Category, Error, Result};
use rusqlite::{params, Connection};
use std::collections::HashSet;

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
