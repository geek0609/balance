//! Carry receivable and payable bills into new books with their original dates.

use crate::inventory::{check_invoice_reference, ledger_category, write_postings, InvoiceKind};
use crate::{
    check_ledgers, opening, parse_date, required, validate_voucher, Category, Error, Posting,
    Result, Store, VoucherKind,
};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE opening_bills(
            invoice_id INTEGER PRIMARY KEY REFERENCES invoices(id),
            original_date TEXT NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct OpeningBillDraft {
    pub kind: InvoiceKind,
    pub party_ledger_id: i64,
    pub reference: String,
    pub original_date: String,
    pub opening_date: String,
    pub due_date: Option<String>,
    pub amount: i64,
}

#[derive(Debug, Clone)]
pub struct OpeningBill {
    pub original_date: String,
    pub amount: i64,
}

pub fn label(kind: InvoiceKind) -> &'static str {
    match kind {
        InvoiceKind::Sale => "Opening receivable",
        InvoiceKind::Purchase => "Opening payable",
    }
}

pub(crate) fn for_invoice(
    conn: &Connection,
    company: i64,
    invoice: i64,
) -> Result<Option<OpeningBill>> {
    Ok(conn
        .query_row(
            "SELECT b.original_date, b.amount FROM opening_bills b
         JOIN invoices i ON i.id = b.invoice_id
         WHERE i.company_id = ?1 AND i.id = ?2",
            params![company, invoice],
            |row| {
                Ok(OpeningBill {
                    original_date: row.get(0)?,
                    amount: row.get(1)?,
                })
            },
        )
        .optional()?)
}

pub(crate) fn ensure_trade_invoice(conn: &Connection, company: i64, invoice: i64) -> Result<()> {
    if for_invoice(conn, company, invoice)?.is_some() {
        return Err(Error::Validation(
            "Opening bills use their own posting and clearing workflow".into(),
        ));
    }
    Ok(())
}

pub(crate) fn check_aggregate_opening(
    conn: &Connection,
    company: i64,
    ledger: i64,
    date: NaiveDate,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT v.date FROM opening_bills b JOIN invoices i ON i.id = b.invoice_id
         LEFT JOIN invoice_reversals r ON r.invoice_id = i.id AND r.company_id = i.company_id
         LEFT JOIN vouchers v ON v.id = r.reversal_voucher_id AND v.company_id = i.company_id
         WHERE i.company_id = ?1 AND i.party_ledger_id = ?2",
    )?;
    for row in stmt.query_map(params![company, ledger], |row| {
        row.get::<_, Option<String>>(0)
    })? {
        let reversal = row?.ok_or_else(|| {
            Error::Validation("This ledger already uses opening bills. Clear active opening bills before entering an aggregate opening balance.".into())
        })?;
        if date < parse_date(&reversal)? {
            return Err(Error::Validation(
                "Opening date cannot precede a clearing entry for this ledger's opening bills"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn check_opening_bill_date(
    conn: &Connection,
    company: i64,
    ledger: i64,
    date: NaiveDate,
) -> Result<()> {
    let balance: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM opening_balances WHERE company_id = ?1 AND ledger_id = ?2)",
        params![company, ledger],
        |row| row.get(0),
    )?;
    if balance {
        return Err(Error::Validation(
            "This ledger already has an aggregate opening balance. Clear it before entering opening bills.".into(),
        ));
    }
    let mut stmt = conn.prepare(
        "SELECT v.date FROM opening_balance_clears c
         JOIN vouchers v ON v.id = c.clearing_voucher_id AND v.company_id = c.company_id
         WHERE c.company_id = ?1 AND c.ledger_id = ?2",
    )?;
    for row in stmt.query_map(params![company, ledger], |row| row.get::<_, String>(0))? {
        if date < parse_date(&row?)? {
            return Err(Error::Validation(
                "Opening date cannot precede a clearing entry for this ledger".into(),
            ));
        }
    }
    Ok(())
}

fn bill_postings(party: i64, control: i64, amount: i64, kind: InvoiceKind) -> [Posting; 2] {
    let debit = match kind {
        InvoiceKind::Sale => amount,
        InvoiceKind::Purchase => -amount,
    };
    [
        Posting {
            ledger_id: party,
            amount: debit,
        },
        Posting {
            ledger_id: control,
            amount: -debit,
        },
    ]
}

pub(crate) fn clearing_postings(
    conn: &Connection,
    company: i64,
    invoice: i64,
    kind: InvoiceKind,
    bill: &OpeningBill,
) -> Result<[Posting; 2]> {
    let party = conn.query_row(
        "SELECT party_ledger_id FROM invoices WHERE id = ?1 AND company_id = ?2",
        params![invoice, company],
        |row| row.get(0),
    )?;
    let control = opening::control_id(conn, company)?
        .ok_or_else(|| Error::Validation("Opening difference ledger is unavailable".into()))?;
    let postings = bill_postings(party, control, -bill.amount, kind);
    check_ledgers(conn, company, &postings)?;
    Ok(postings)
}

impl Store {
    pub fn post_opening_bill(&mut self, company: i64, draft: &OpeningBillDraft) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let invoice = insert_opening_bill(&tx, company, draft)?;
        tx.commit()?;
        Ok(invoice)
    }
}

pub(crate) fn insert_opening_bill(
    conn: &Connection,
    company: i64,
    draft: &OpeningBillDraft,
) -> Result<i64> {
    let reference = required(&draft.reference, "Original bill reference")?;
    let original = parse_date(&draft.original_date)?;
    let opening = parse_date(&draft.opening_date)?;
    let due = draft.due_date.as_deref().map(parse_date).transpose()?;
    if original > opening || due.is_some_and(|due| due < original) {
        return Err(Error::Validation(
            "The original bill date must be on or before its opening date and due date".into(),
        ));
    }
    if draft.amount <= 0 {
        return Err(Error::Validation(
            "Enter a positive outstanding bill amount".into(),
        ));
    }
    let expected_category = match draft.kind {
        InvoiceKind::Sale => Category::Asset,
        InvoiceKind::Purchase => Category::Liability,
    };
    if ledger_category(conn, company, draft.party_ledger_id)? != expected_category {
        return Err(Error::Validation(
            "Receivables require an asset ledger and payables require a liability ledger".into(),
        ));
    }
    check_opening_bill_date(conn, company, draft.party_ledger_id, opening)?;
    check_invoice_reference(
        conn,
        company,
        draft.kind,
        draft.party_ledger_id,
        reference,
        None,
    )?;
    let control = opening::ensure_control(conn, company)?;
    let postings = bill_postings(draft.party_ledger_id, control, draft.amount, draft.kind);
    let date = opening.to_string();
    validate_voucher(&date, &postings)?;
    check_ledgers(conn, company, &postings)?;

    conn.execute(
        "INSERT INTO vouchers(company_id, kind, reference, date, narration)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            company,
            VoucherKind::Journal.as_str(),
            reference,
            date,
            label(draft.kind)
        ],
    )?;
    let voucher = conn.last_insert_rowid();
    write_postings(conn, voucher, &postings)?;
    conn.execute(
        "INSERT INTO invoices(company_id, voucher_id, kind, party_ledger_id, trade_ledger_id, tax_amount, due_date)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
        params![company, voucher, draft.kind.as_str(), draft.party_ledger_id, control, due.map(|d| d.to_string())],
    )?;
    let invoice = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO opening_bills(invoice_id, original_date, amount) VALUES (?1, ?2, ?3)",
        params![invoice, original.to_string(), draft.amount],
    )?;
    Ok(invoice)
}
