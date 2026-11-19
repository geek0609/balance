//! Cash receipts and payments that use named note credit.
use crate::bill_note_credits::read_credits;
use crate::inventory::{ledger_category, InvoiceKind};
use crate::settlement_discounts::postings;
use crate::{
    insert_voucher_draft, params, parse_date, required, Category, Connection, Error, Result, Store,
    VoucherDraft, VoucherKind,
};
use chrono::NaiveDate;
use rusqlite::TransactionBehavior;
use std::collections::HashSet;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS bill_note_refunds(
            credit_id INTEGER NOT NULL REFERENCES bill_note_credits(id),
            voucher_id INTEGER NOT NULL REFERENCES vouchers(id),
            bank_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            PRIMARY KEY(credit_id, voucher_id)
        );
        CREATE INDEX IF NOT EXISTS bill_note_refunds_voucher ON bill_note_refunds(voucher_id);
        CREATE VIEW IF NOT EXISTS bill_note_refund_history AS
          SELECT f.credit_id, f.voucher_id, f.bank_ledger_id, f.amount, v.company_id, v.date, v.reference,
                 COALESCE(rv.date, sv.date, av.date) AS reversal_date
          FROM bill_note_refunds f JOIN vouchers v ON v.id = f.voucher_id
          LEFT JOIN voucher_reversals r ON r.original_voucher_id = v.id AND r.company_id = v.company_id
          LEFT JOIN settlement_reversals s ON s.settlement_voucher_id = v.id AND s.company_id = v.company_id
          LEFT JOIN advance_vouchers a ON a.voucher_id = v.id AND a.company_id = v.company_id
          LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
          LEFT JOIN vouchers sv ON sv.id = s.reversal_voucher_id
          LEFT JOIN vouchers av ON av.id = a.reversal_voucher_id;",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct NoteRefundAllocation {
    pub credit_id: i64,
    pub amount: i64,
}

#[derive(Debug, Clone)]
pub struct NoteRefund {
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub bank: String,
    pub amount: i64,
    pub reversal_date: Option<String>,
}

pub(crate) fn opposite_kind(kind: InvoiceKind) -> InvoiceKind {
    match kind {
        InvoiceKind::Sale => InvoiceKind::Purchase,
        InvoiceKind::Purchase => InvoiceKind::Sale,
    }
}

pub(crate) fn validate_refunds(
    conn: &Connection,
    company: i64,
    kind: InvoiceKind,
    party: i64,
    date: NaiveDate,
    allocations: &[NoteRefundAllocation],
) -> Result<i64> {
    let mut seen = HashSet::new();
    let mut total = 0_i128;
    for allocation in allocations {
        if allocation.amount <= 0 || !seen.insert(allocation.credit_id) {
            return Err(Error::Validation(
                "Each refund needs a distinct credit and a positive amount".into(),
            ));
        }
        let credit = read_credits(conn, company, Some(allocation.credit_id), None)?
            .pop()
            .ok_or_else(|| Error::Validation("Select a named credit in this company".into()))?;
        if credit.kind != kind || credit.party_ledger_id != party {
            return Err(Error::Validation(
                "Refunds must use credits for the same party and direction".into(),
            ));
        }
        if date < parse_date(&credit.date)?
            || credit.latest_release.is_some_and(|release| date < release)
        {
            return Err(Error::Validation(
                "Refund cannot precede the credit or its latest release or reversal".into(),
            ));
        }
        if i128::from(allocation.amount) > credit.available {
            return Err(Error::Validation(
                "Refund exceeds the available credit".into(),
            ));
        }
        total += i128::from(allocation.amount);
    }
    i64::try_from(total).map_err(|_| Error::Validation("Refund total is too large".into()))
}

pub(crate) fn post_refund_voucher(
    conn: &Connection,
    company: i64,
    cash_kind: InvoiceKind,
    bank: i64,
    party: i64,
    date: &str,
    reference: &str,
    narration: &str,
    total: i64,
) -> Result<i64> {
    if bank == party || ledger_category(conn, company, bank)? != Category::Asset {
        return Err(Error::Validation(
            "Select a cash or bank asset ledger different from the party".into(),
        ));
    }
    let kind = match cash_kind {
        InvoiceKind::Sale => VoucherKind::Receipt,
        InvoiceKind::Purchase => VoucherKind::Payment,
    };
    let postings = postings(
        conn,
        company,
        cash_kind,
        bank,
        party,
        0,
        i128::from(total),
        &[],
    )?;
    insert_voucher_draft(
        conn,
        company,
        VoucherDraft {
            kind,
            date,
            reference,
            narration,
            postings: &postings,
            party_lines: &[1],
        },
    )
}

pub(crate) fn attach_refunds(
    conn: &Connection,
    voucher: i64,
    bank: i64,
    allocations: &[NoteRefundAllocation],
) -> Result<()> {
    if !allocations.is_empty() {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO bill_note_refunds(credit_id, voucher_id, bank_ledger_id, amount) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for allocation in allocations {
            stmt.execute(params![
                allocation.credit_id,
                voucher,
                bank,
                allocation.amount
            ])?;
        }
    }
    Ok(())
}

pub(crate) fn total_for_voucher(conn: &Connection, voucher: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(amount), 0) FROM bill_note_refunds WHERE voucher_id = ?1",
        [voucher],
        |row| row.get(0),
    )?)
}

pub(crate) fn is_linked(conn: &Connection, company: i64, voucher: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bill_note_refunds f JOIN vouchers v ON v.id = f.voucher_id
         WHERE f.voucher_id = ?1 AND v.company_id = ?2)",
        params![voucher, company],
        |row| row.get(0),
    )?)
}

impl Store {
    pub fn is_named_credit_refund(&self, company: i64, voucher: i64) -> Result<bool> {
        is_linked(&self.conn, company, voucher)
    }

    pub fn bill_note_refunds(&self, company: i64, credit: i64) -> Result<Vec<NoteRefund>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.voucher_id, f.date, f.reference, l.name, f.amount, f.reversal_date
             FROM bill_note_refund_history f JOIN ledgers l ON l.id = f.bank_ledger_id
             WHERE f.company_id = ?1 AND f.credit_id = ?2 ORDER BY f.date, f.voucher_id",
        )?;
        let rows = stmt.query_map(params![company, credit], |row| {
            Ok(NoteRefund {
                voucher_id: row.get(0)?,
                date: row.get(1)?,
                reference: row.get(2)?,
                bank: row.get(3)?,
                amount: row.get(4)?,
                reversal_date: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn refund_bill_note_credits(
        &mut self,
        company: i64,
        bank: i64,
        date: &str,
        reference: &str,
        allocations: &[NoteRefundAllocation],
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Refund reference")?;
        let first = allocations
            .first()
            .ok_or_else(|| Error::Validation("Select at least one credit".into()))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::advances::check_open_date(&tx, company, date)?;
        let credit = read_credits(&tx, company, Some(first.credit_id), None)?
            .pop()
            .ok_or_else(|| Error::Validation("Select a named credit in this company".into()))?;
        let total = validate_refunds(
            &tx,
            company,
            credit.kind,
            credit.party_ledger_id,
            date,
            allocations,
        )?;
        let date = date.to_string();
        let cash_kind = opposite_kind(credit.kind);
        let kind = if cash_kind == InvoiceKind::Sale {
            VoucherKind::Receipt
        } else {
            VoucherKind::Payment
        };
        crate::check_new_bill_voucher(&tx, company, kind, &date, reference)?;
        let voucher = post_refund_voucher(
            &tx,
            company,
            cash_kind,
            bank,
            credit.party_ledger_id,
            &date,
            reference,
            "Refund against named note credit",
            total,
        )?;
        attach_refunds(&tx, voucher, bank, allocations)?;
        tx.commit()?;
        Ok(voucher)
    }
}
