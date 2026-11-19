//! Bill schedules and their dated receipt or payment allocations.

use crate::{parse_date, required, Error, Result};
use chrono::{Days, NaiveDate};
use rusqlite::{params, Connection};
use std::collections::HashSet;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE invoice_bill_splits(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            reference TEXT NOT NULL CHECK(length(trim(reference)) > 0),
            amount INTEGER NOT NULL CHECK(amount > 0),
            due_date TEXT,
            credit_days INTEGER,
            UNIQUE(invoice_id, reference),
            UNIQUE(id, invoice_id)
        );
        CREATE TABLE invoice_bill_split_revisions(
            id INTEGER PRIMARY KEY,
            revision_id INTEGER NOT NULL REFERENCES invoice_revisions(id),
            reference TEXT NOT NULL,
            amount INTEGER NOT NULL,
            due_date TEXT,
            credit_days INTEGER
        );
        CREATE UNIQUE INDEX invoice_settlements_with_invoice ON invoice_settlements(id, invoice_id);
        CREATE TABLE bill_split_settlements(
            settlement_id INTEGER NOT NULL,
            split_id INTEGER NOT NULL,
            invoice_id INTEGER NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0),
            PRIMARY KEY(settlement_id, split_id),
            FOREIGN KEY(settlement_id, invoice_id) REFERENCES invoice_settlements(id, invoice_id),
            FOREIGN KEY(split_id, invoice_id) REFERENCES invoice_bill_splits(id, invoice_id)
        );
        CREATE INDEX bill_split_settlements_split ON bill_split_settlements(split_id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct BillSplitDraft {
    pub reference: String,
    /// Amount in hundredths.
    pub amount: i64,
    pub due_date: Option<String>,
    pub credit_days: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct BillSplit {
    pub id: i64,
    pub reference: String,
    pub amount: i64,
    pub due_date: Option<String>,
    pub credit_days: Option<u32>,
    pub settled: i128,
}

pub(crate) fn prepare(
    splits: &[BillSplitDraft],
    invoice_date: NaiveDate,
    total: i64,
) -> Result<Vec<BillSplitDraft>> {
    if splits.is_empty() {
        return Ok(Vec::new());
    }
    if splits.len() < 2 {
        return Err(Error::Validation(
            "Enter at least two bills in the split".into(),
        ));
    }
    if splits.iter().any(|bill| bill.amount <= 0)
        || splits
            .iter()
            .map(|bill| i128::from(bill.amount))
            .sum::<i128>()
            != i128::from(total)
    {
        return Err(Error::Validation(
            "Split bill amounts must be positive and add up to the document total".into(),
        ));
    }
    let mut seen = HashSet::new();
    splits
        .iter()
        .map(|bill| {
            let reference = required(&bill.reference, "Split bill reference")?;
            if !seen.insert(reference.to_lowercase()) {
                return Err(Error::Validation(
                    "Each bill needs a distinct reference".into(),
                ));
            }
            let date = match (bill.due_date.as_deref(), bill.credit_days) {
                (Some(_), Some(_)) => {
                    return Err(Error::Validation(
                        "Enter either a due date or credit days for each bill".into(),
                    ))
                }
                (Some(date), None) => Some(parse_date(date)?),
                (None, Some(days)) => Some(
                    invoice_date
                        .checked_add_days(Days::new(u64::from(days)))
                        .ok_or_else(|| {
                            Error::Validation("Credit days produce an invalid due date".into())
                        })?,
                ),
                (None, None) => None,
            };
            Ok(BillSplitDraft {
                reference: reference.into(),
                amount: bill.amount,
                due_date: date.map(|value| value.to_string()),
                credit_days: bill.credit_days,
            })
        })
        .collect()
}

pub(crate) fn write(conn: &Connection, invoice: i64, bills: &[BillSplitDraft]) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO invoice_bill_splits(invoice_id, reference, amount, due_date, credit_days)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for bill in bills {
        stmt.execute(params![
            invoice,
            bill.reference,
            bill.amount,
            bill.due_date,
            bill.credit_days
        ])?;
    }
    Ok(())
}

pub(crate) fn read(conn: &Connection, invoice: i64, as_of: Option<&str>) -> Result<Vec<BillSplit>> {
    let mut stmt = conn.prepare(
        "SELECT b.id, b.reference, b.amount, b.due_date, b.credit_days,
           COALESCE((SELECT SUM(a.amount) FROM bill_split_settlements a
             JOIN invoice_settlements s ON s.id = a.settlement_id
             JOIN vouchers v ON v.id = s.voucher_id
             LEFT JOIN settlement_reversals r ON r.settlement_voucher_id = s.voucher_id
             LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
             WHERE a.split_id = b.id AND (?2 IS NULL OR v.date <= ?2)
               AND (rv.id IS NULL OR (?2 IS NOT NULL AND rv.date > ?2))), 0)
         FROM invoice_bill_splits b WHERE b.invoice_id = ?1 ORDER BY b.id",
    )?;
    let rows = stmt
        .query_map(params![invoice, as_of], |row| {
            Ok(BillSplit {
                id: row.get(0)?,
                reference: row.get(1)?,
                amount: row.get(2)?,
                due_date: row.get(3)?,
                credit_days: row.get(4)?,
                settled: i128::from(row.get::<_, i64>(5)?),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn require_unsplit(conn: &Connection, invoice: i64) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM invoice_bill_splits WHERE invoice_id = ?1)",
        [invoice],
        |row| row.get(0),
    )?;
    if exists {
        return Err(Error::Validation(
            "Credits and advances on a split bill schedule require individual bill allocations"
                .into(),
        ));
    }
    Ok(())
}

pub(crate) fn revisions(conn: &Connection, revision: i64) -> Result<Vec<BillSplitDraft>> {
    let mut stmt = conn.prepare(
        "SELECT reference, amount, due_date, credit_days
         FROM invoice_bill_split_revisions WHERE revision_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([revision], |row| {
            Ok(BillSplitDraft {
                reference: row.get(0)?,
                amount: row.get(1)?,
                due_date: row.get(2)?,
                credit_days: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn settlement_allocations(
    conn: &Connection,
    settlement: i64,
) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT b.reference, a.amount FROM bill_split_settlements a
         JOIN invoice_bill_splits b ON b.id = a.split_id AND b.invoice_id = a.invoice_id
         WHERE a.settlement_id = ?1 ORDER BY b.id",
    )?;
    let rows = stmt
        .query_map([settlement], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
