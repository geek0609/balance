//! Bill allocations on financial notes with their own ledger postings.
use crate::{params, Connection, Result, Store};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS bill_note_allocations(
            id INTEGER PRIMARY KEY,
            voucher_id INTEGER NOT NULL REFERENCES vouchers(id),
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            split_id INTEGER,
            amount INTEGER NOT NULL CHECK(amount > 0),
            FOREIGN KEY(split_id, invoice_id) REFERENCES invoice_bill_splits(id, invoice_id)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS bill_note_allocation_target
            ON bill_note_allocations(voucher_id, invoice_id, COALESCE(split_id, 0));
        CREATE INDEX IF NOT EXISTS bill_note_allocations_invoice ON bill_note_allocations(invoice_id);
        CREATE INDEX IF NOT EXISTS bill_note_allocations_split ON bill_note_allocations(split_id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct BillNoteAllocation {
    pub voucher_id: i64,
    pub reference: String,
    pub date: String,
    pub bill_reference: String,
    pub amount: i64,
    pub reversal_date: Option<String>,
}

pub(crate) fn history(
    conn: &Connection,
    company: i64,
    invoice: i64,
) -> Result<Vec<BillNoteAllocation>> {
    let mut stmt = conn.prepare_cached(
        "SELECT v.id, v.reference, v.date, COALESCE(b.reference, i.bill_reference, iv.reference),
                a.amount, rv.date
         FROM bill_note_allocations a JOIN invoices i ON i.id = a.invoice_id
         JOIN vouchers iv ON iv.id = i.voucher_id
         JOIN vouchers v ON v.id = a.voucher_id AND v.company_id = i.company_id
         LEFT JOIN invoice_bill_splits b ON b.id = a.split_id
         LEFT JOIN voucher_reversals r ON r.original_voucher_id = v.id AND r.company_id = v.company_id
         LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
         WHERE i.company_id = ?1 AND i.id = ?2 ORDER BY v.date, v.id, a.id",
    )?;
    let rows = stmt.query_map(params![company, invoice], |row| {
        Ok(BillNoteAllocation {
            voucher_id: row.get(0)?,
            reference: row.get(1)?,
            date: row.get(2)?,
            bill_reference: row.get(3)?,
            amount: row.get(4)?,
            reversal_date: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn is_bill_note(conn: &Connection, company: i64, voucher: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bill_note_allocations a JOIN vouchers v ON v.id = a.voucher_id
         WHERE a.voucher_id = ?1 AND v.company_id = ?2)",
        params![voucher, company],
        |row| row.get(0),
    )?)
}

impl Store {
    pub fn bill_note_allocations(
        &self,
        company: i64,
        invoice: i64,
    ) -> Result<Vec<BillNoteAllocation>> {
        history(&self.conn, company, invoice)
    }

    pub fn is_bill_note(&self, company: i64, voucher: i64) -> Result<bool> {
        is_bill_note(&self.conn, company, voucher)
    }
}
