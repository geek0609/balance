//! Noncash application and release of credit available on an invoice note.

use crate::advances::{read_allocation_state, AllocationState};
use crate::inventory::BillSettlementTarget;
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE invoice_credit_allocations(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            note_id INTEGER NOT NULL REFERENCES invoice_refund_credits(note_id),
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            date TEXT NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0),
            released_on TEXT,
            release_reason TEXT,
            CHECK((released_on IS NULL) = (release_reason IS NULL))
        );
        CREATE INDEX invoice_credit_allocations_invoice ON invoice_credit_allocations(company_id, invoice_id);
        CREATE INDEX invoice_credit_allocations_note ON invoice_credit_allocations(company_id, note_id);",
    )?;
    Ok(())
}

pub(crate) fn state(
    conn: &Connection,
    company: i64,
    invoice: i64,
    through: Option<NaiveDate>,
) -> Result<AllocationState> {
    let mut stmt = conn.prepare_cached(
        "SELECT date, amount, released_on FROM invoice_credit_allocations WHERE company_id = ?1 AND invoice_id = ?2
         UNION ALL SELECT a.date, a.amount, a.released_on FROM bill_note_credit_applications a
           JOIN bill_note_credits c ON c.id = a.credit_id JOIN vouchers v ON v.id = c.voucher_id
           WHERE v.company_id = ?1 AND a.invoice_id = ?2",
    )?;
    let rows = stmt.query(params![company, invoice])?;
    read_allocation_state(rows, through)
}

#[derive(Debug, Clone)]
pub struct CreditAllocation {
    pub id: i64,
    pub invoice_reference: String,
    pub bill_reference: String,
    pub date: String,
    pub amount: i64,
    pub released_on: Option<String>,
    pub release_reason: Option<String>,
}

impl Store {
    pub fn invoice_credit_allocations(
        &self,
        company: i64,
        note: i64,
    ) -> Result<Vec<CreditAllocation>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.id, v.reference, a.date, a.amount, a.released_on, a.release_reason,
                    COALESCE(b.reference, i.bill_reference, v.reference)
             FROM invoice_credit_allocations a JOIN invoices i ON i.id = a.invoice_id AND i.company_id = a.company_id
             JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
             LEFT JOIN bill_split_credit_allocations s ON s.allocation_id = a.id AND s.invoice_id = i.id
             LEFT JOIN invoice_bill_splits b ON b.id = s.split_id AND b.invoice_id = i.id
             WHERE a.company_id = ?1 AND a.note_id = ?2 ORDER BY a.date, a.id",
        )?;
        let rows = stmt
            .query_map(params![company, note], |row| {
                Ok(CreditAllocation {
                    id: row.get(0)?,
                    invoice_reference: row.get(1)?,
                    bill_reference: row.get(6)?,
                    date: row.get(2)?,
                    amount: row.get(3)?,
                    released_on: row.get(4)?,
                    release_reason: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn apply_invoice_credit(
        &mut self,
        company: i64,
        note: i64,
        invoice: i64,
        date: &str,
        amount: i64,
    ) -> Result<i64> {
        self.apply_invoice_credit_to_bill(
            company,
            note,
            BillSettlementTarget {
                invoice_id: invoice,
                split_id: None,
            },
            date,
            amount,
        )
    }

    pub fn apply_invoice_credit_to_bill(
        &mut self,
        company: i64,
        note: i64,
        target: BillSettlementTarget,
        date: &str,
        amount: i64,
    ) -> Result<i64> {
        let invoice = target.invoice_id;
        let date = parse_date(date)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::advances::check_open_date(&tx, company, date)?;
        let (kind, party, credit) =
            crate::invoice_refunds::available_credit(&tx, company, note, date)?;
        if amount <= 0 || i128::from(amount) > credit {
            return Err(Error::Validation(
                "Enter a positive amount within the available credit".into(),
            ));
        }
        let unpaid = crate::invoice_credits::unpaid_for_allocation(
            &tx, company, invoice, kind, party, date,
        )?;
        let bill = crate::bill_splits::target(&tx, target)?;
        let unpaid = bill
            .as_ref()
            .map_or(unpaid, |bill| unpaid.min(bill.outstanding()));
        if i128::from(amount) > unpaid {
            return Err(Error::Validation(
                "Credit allocation exceeds the outstanding bill balance".into(),
            ));
        }
        tx.execute(
            "INSERT INTO invoice_credit_allocations(company_id, note_id, invoice_id, date, amount) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![company, note, invoice, date.to_string(), amount],
        )?;
        let id = tx.last_insert_rowid();
        if let Some(bill) = bill {
            tx.execute(
                "INSERT INTO bill_split_credit_allocations(allocation_id, split_id, invoice_id)
                 VALUES (?1, ?2, ?3)",
                params![id, bill.id, invoice],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn release_invoice_credit(
        &mut self,
        company: i64,
        allocation: i64,
        date: &str,
        reason: &str,
    ) -> Result<()> {
        let date = parse_date(date)?;
        let reason = required(reason, "Credit release reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::advances::check_open_date(&tx, company, date)?;
        let original = tx.query_row(
            "SELECT date FROM invoice_credit_allocations WHERE company_id = ?1 AND id = ?2 AND released_on IS NULL",
            params![company, allocation], |row| row.get::<_, String>(0),
        ).optional()?.ok_or_else(|| Error::Validation("Select an active credit allocation".into()))?;
        if date < parse_date(&original)? {
            return Err(Error::Validation(
                "Credit release date cannot precede the allocation".into(),
            ));
        }
        tx.execute(
            "UPDATE invoice_credit_allocations SET released_on = ?1, release_reason = ?2 WHERE company_id = ?3 AND id = ?4",
            params![date.to_string(), reason, company, allocation],
        )?;
        tx.commit()?;
        Ok(())
    }
}
