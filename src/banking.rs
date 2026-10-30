//! Manual clearance of cash and bank ledger postings.
use crate::{parse_date, required, Category, Error, Result, Store};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct BankEntry {
    pub posting_id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub narration: String,
    pub amount: i64,
    pub cleared_on: Option<String>,
}
#[derive(Debug, Clone)]
pub struct BankReport {
    pub entries: Vec<BankEntry>,
    pub book_balance: i128,
    pub cleared_balance: i128,
    pub uncleared: i128,
}
#[derive(Debug, Clone)]
pub struct ClearanceEvent {
    pub changed_at: String,
    pub posting_id: i64,
    pub ledger: String,
    pub amount: i64,
    pub voucher_date: String,
    pub previous_date: Option<String>,
    pub new_date: Option<String>,
    pub reason: String,
}

pub(crate) fn ensure_no_bank_clearance(
    conn: &Connection,
    company: i64,
    voucher: i64,
) -> Result<()> {
    let cleared: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM bank_clearances c JOIN postings p ON p.id = c.posting_id WHERE c.company_id = ?1 AND p.voucher_id = ?2)", params![company, voucher], |r| r.get(0))?;
    if cleared {
        return Err(Error::Validation(
            "Remove the bank clearance before editing this voucher".into(),
        ));
    }
    Ok(())
}
fn asset_ledger(conn: &Connection, company: i64, ledger: i64) -> Result<()> {
    let category: Option<String> = conn
        .query_row(
            "SELECT category FROM ledgers WHERE id = ?1 AND company_id = ?2",
            params![ledger, company],
            |r| r.get(0),
        )
        .optional()?;
    if category.as_deref() != Some(Category::Asset.as_str()) {
        return Err(Error::Validation(
            "Select an asset ledger in the selected company".into(),
        ));
    }
    Ok(())
}
impl Store {
    /// Report book postings through an inclusive date, using current clearance dates.
    pub fn bank_report(&self, company: i64, ledger: i64, as_of: &str) -> Result<BankReport> {
        asset_ledger(&self.conn, company, ledger)?;
        let end = parse_date(as_of)?;
        let mut stmt = self.conn.prepare("SELECT p.id, v.id, v.date, v.reference, v.narration, p.amount, c.cleared_on FROM postings p JOIN vouchers v ON v.id = p.voucher_id LEFT JOIN bank_clearances c ON c.posting_id = p.id AND c.company_id = v.company_id WHERE v.company_id = ?1 AND p.ledger_id = ?2 ORDER BY v.date, v.id, p.id")?;
        let rows = stmt.query_map(params![company, ledger], |r| {
            Ok(BankEntry {
                posting_id: r.get(0)?,
                voucher_id: r.get(1)?,
                date: r.get(2)?,
                reference: r.get(3)?,
                narration: r.get(4)?,
                amount: r.get(5)?,
                cleared_on: r.get(6)?,
            })
        })?;
        let mut entries = Vec::new();
        let mut book = 0_i128;
        let mut cleared = 0_i128;
        for row in rows {
            let entry = row?;
            if parse_date(&entry.date)? > end {
                continue;
            }
            book += i128::from(entry.amount);
            if let Some(date) = &entry.cleared_on {
                if parse_date(date)? <= end {
                    cleared += i128::from(entry.amount);
                }
            }
            entries.push(entry);
        }
        Ok(BankReport {
            entries,
            book_balance: book,
            cleared_balance: cleared,
            uncleared: book - cleared,
        })
    }
    /// Set or remove a clearance; every change records the prior value and posting snapshot.
    pub fn set_bank_clearance(
        &mut self,
        company: i64,
        ledger: i64,
        posting: i64,
        date: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason")?;
        let new_date = date
            .map(parse_date)
            .transpose()?
            .map(|d| d.format("%Y-%m-%d").to_string());
        let tx = self.conn.transaction()?;
        asset_ledger(&tx, company, ledger)?;
        let (voucher, voucher_date, amount): (i64, String, i64) = tx.query_row("SELECT v.id, v.date, p.amount FROM postings p JOIN vouchers v ON v.id = p.voucher_id WHERE p.id = ?1 AND p.ledger_id = ?2 AND v.company_id = ?3", params![posting, ledger, company], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Posting not found in this ledger and company".into()), other => Error::Database(other) })?;
        if let Some(new_date) = &new_date {
            if parse_date(new_date)? < parse_date(&voucher_date)? {
                return Err(Error::Validation(
                    "Clearance date must not precede the voucher date".into(),
                ));
            }
        }
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT company_id, cleared_on FROM bank_clearances WHERE posting_id = ?1",
                [posting],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if existing
            .as_ref()
            .is_some_and(|(owner, _)| *owner != company)
        {
            return Err(Error::Validation(
                "Clearance has an inconsistent company".into(),
            ));
        }
        let previous = existing.map(|(_, date)| date);
        if previous == new_date {
            return Err(Error::Validation("Clearance has not changed".into()));
        }
        tx.execute("INSERT INTO bank_clearance_events(company_id, voucher_id, posting_id, ledger_id, amount, voucher_date, previous_date, new_date, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)", params![company, voucher, posting, ledger, amount, voucher_date, previous, new_date, reason])?;
        if let Some(value) = new_date {
            tx.execute("INSERT INTO bank_clearances(posting_id, company_id, cleared_on) VALUES (?1, ?2, ?3) ON CONFLICT(posting_id) DO UPDATE SET cleared_on = excluded.cleared_on", params![posting, company, value])?;
        } else {
            tx.execute(
                "DELETE FROM bank_clearances WHERE posting_id = ?1 AND company_id = ?2",
                params![posting, company],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn clearance_events(&self, company: i64, voucher: i64) -> Result<Vec<ClearanceEvent>> {
        let mut stmt = self.conn.prepare("SELECT e.changed_at, e.posting_id, l.name, e.amount, e.voucher_date, e.previous_date, e.new_date, e.reason FROM bank_clearance_events e JOIN ledgers l ON l.id = e.ledger_id AND l.company_id = e.company_id WHERE e.company_id = ?1 AND e.voucher_id = ?2 ORDER BY e.id DESC")?;
        let rows = stmt
            .query_map(params![company, voucher], |r| {
                Ok(ClearanceEvent {
                    changed_at: r.get(0)?,
                    posting_id: r.get(1)?,
                    ledger: r.get(2)?,
                    amount: r.get(3)?,
                    voucher_date: r.get(4)?,
                    previous_date: r.get(5)?,
                    new_date: r.get(6)?,
                    reason: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
