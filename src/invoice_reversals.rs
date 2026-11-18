//! Full invoice reversals preserve the invoice and post dated counter-entries.
use crate::inventory::InvoiceKind;
use crate::{parse_date, required, Error, Result, Store, VoucherKind};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE invoice_reversals(
            invoice_id INTEGER PRIMARY KEY REFERENCES invoices(id),
            company_id INTEGER NOT NULL REFERENCES companies(id),
            reversal_voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id),
            reason TEXT NOT NULL
        );
        CREATE TABLE invoice_reversal_stock(
            invoice_id INTEGER NOT NULL REFERENCES invoice_reversals(invoice_id),
            allocation_id INTEGER NOT NULL UNIQUE REFERENCES invoice_line_allocations(id),
            adjustment_id INTEGER NOT NULL UNIQUE REFERENCES stock_adjustments(id),
            PRIMARY KEY(invoice_id, allocation_id)
        );
        DROP VIEW order_line_progress;
        CREATE VIEW order_line_progress AS
            SELECT ol.id, ol.order_id, ol.item_id, ol.quantity, ol.unit_price, COALESCE((
                SELECT SUM(il.quantity)
                FROM invoice_order_links link JOIN invoice_lines il ON il.invoice_id = link.invoice_id
                WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                    AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
            ), 0) AS fulfilled
            FROM order_lines ol;",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct InvoiceReversal {
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub reason: String,
}

pub(crate) fn for_invoice(
    conn: &Connection,
    company: i64,
    invoice: i64,
) -> Result<Option<InvoiceReversal>> {
    Ok(conn
        .query_row(
            "SELECT v.id, v.date, v.reference, r.reason
         FROM invoice_reversals r JOIN vouchers v ON v.id = r.reversal_voucher_id
         WHERE r.company_id = ?1 AND r.invoice_id = ?2",
            params![company, invoice],
            |row| {
                Ok(InvoiceReversal {
                    voucher_id: row.get(0)?,
                    date: row.get(1)?,
                    reference: row.get(2)?,
                    reason: row.get(3)?,
                })
            },
        )
        .optional()?)
}

impl Store {
    /// Reverse the entire invoice, including stock, with one dated credit or debit note.
    pub fn reverse_invoice(
        &mut self,
        company: i64,
        invoice: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Note reference")?;
        let reason = required(reason, "Reason for reversing the invoice")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (kind, original, invoice_date): (String, i64, String) = tx
            .query_row(
                "SELECT i.kind, v.id, v.date FROM invoices i
             JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
             WHERE i.id = ?1 AND i.company_id = ?2",
                params![invoice, company],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Invoice not found in this company".into()))?;
        if date < parse_date(&invoice_date)? {
            return Err(Error::Validation(
                "Note date cannot precede the invoice date".into(),
            ));
        }
        if for_invoice(&tx, company, invoice)?.is_some() {
            return Err(Error::Validation(
                "This invoice has already been reversed".into(),
            ));
        }
        {
            let mut stmt = tx.prepare(
                "SELECT v.date FROM invoice_settlements s
                 LEFT JOIN settlement_reversals r ON r.settlement_voucher_id = s.voucher_id AND r.company_id = s.company_id
                 LEFT JOIN vouchers v ON v.id = r.reversal_voucher_id AND v.company_id = s.company_id
                 WHERE s.company_id = ?1 AND s.invoice_id = ?2",
            )?;
            for row in stmt.query_map(params![company, invoice], |row| {
                row.get::<_, Option<String>>(0)
            })? {
                let reversal = row?.ok_or_else(|| {
                    Error::Validation(
                        "Reverse the active receipts or payments before reversing this invoice"
                            .into(),
                    )
                })?;
                if date < parse_date(&reversal)? {
                    return Err(Error::Validation(
                        "Note date cannot precede a settlement reversal".into(),
                    ));
                }
            }
        }
        let (voucher_kind, direction) = match InvoiceKind::parse(&kind)? {
            InvoiceKind::Sale => (VoucherKind::CreditNote, 1),
            InvoiceKind::Purchase => (VoucherKind::DebitNote, -1),
        };
        let date = date.format("%Y-%m-%d").to_string();
        let postings = crate::reversals::counter_postings(&tx, company, original, &date)?;
        let note_reason = format!("Invoice {invoice} reversal: {reason}");
        tx.execute(
            "INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![company, voucher_kind.as_str(), reference, date, note_reason],
        )?;
        let voucher = tx.last_insert_rowid();
        for posting in postings {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![voucher, posting.ledger_id, posting.amount],
            )?;
        }
        tx.execute(
            "INSERT INTO invoice_reversals(invoice_id, company_id, reversal_voucher_id, reason) VALUES (?1, ?2, ?3, ?4)",
            params![invoice, company, voucher, reason],
        )?;
        {
            let mut stmt = tx.prepare(
                "SELECT a.id, l.item_id, a.location_id, a.batch_id, a.quantity
                 FROM invoice_lines l JOIN invoice_line_allocations a ON a.line_id = l.id
                 WHERE l.invoice_id = ?1 ORDER BY l.id, a.id",
            )?;
            let mut rows = stmt.query([invoice])?;
            while let Some(row) = rows.next()? {
                let allocation: i64 = row.get(0)?;
                let item: i64 = row.get(1)?;
                let location: i64 = row.get(2)?;
                let batch: Option<i64> = row.get(3)?;
                let quantity: i64 = row.get(4)?;
                tx.execute(
                    "INSERT INTO stock_adjustments(company_id, item_id, date, quantity, reason, location_id, batch_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![company, item, date, direction * quantity, note_reason, location, batch],
                )?;
                tx.execute(
                    "INSERT INTO invoice_reversal_stock(invoice_id, allocation_id, adjustment_id) VALUES (?1, ?2, ?3)",
                    params![invoice, allocation, tx.last_insert_rowid()],
                )?;
            }
        }
        tx.commit()?;
        Ok(voucher)
    }
}
