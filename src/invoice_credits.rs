//! Dated financial credits against unsettled portions of posted invoices.

use crate::inventory::{invoice_total, InvoiceKind};
use crate::{
    insert_voucher_draft, parse_date, required, Error, Posting, Result, Store, VoucherDraft,
    VoucherKind,
};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE invoice_credit_notes(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id),
            reference_key TEXT NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0),
            reason TEXT NOT NULL,
            reversal_voucher_id INTEGER UNIQUE REFERENCES vouchers(id),
            reversal_reason TEXT,
            CHECK((reversal_voucher_id IS NULL) = (reversal_reason IS NULL)),
            UNIQUE(invoice_id, reference_key)
        );
        CREATE INDEX invoice_credit_notes_invoice ON invoice_credit_notes(invoice_id, id);
        CREATE TABLE invoice_credit_lines(
            note_id INTEGER NOT NULL REFERENCES invoice_credit_notes(id),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            is_tax INTEGER NOT NULL CHECK(is_tax IN (0, 1)),
            reduction INTEGER NOT NULL CHECK(reduction != 0 AND (is_tax = 0 OR reduction > 0)),
            PRIMARY KEY(note_id, ledger_id, is_tax)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CreditComponent {
    pub ledger_id: i64,
    pub ledger: String,
    pub is_tax: bool,
    /// Positive reductions decrease the bill. Negative reductions reverse an original deduction.
    pub remaining: i128,
}

#[derive(Debug, Clone)]
pub struct CreditLineDraft {
    pub ledger_id: i64,
    pub is_tax: bool,
    pub amount: i64,
}

#[derive(Debug, Clone)]
pub struct CreditNote {
    pub id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub amount: i64,
    pub refundable: i64,
    pub reason: String,
    pub reversal_voucher_id: Option<i64>,
    pub reversal_date: Option<String>,
    pub reversal_reason: Option<String>,
}

#[derive(Default)]
pub(crate) struct CreditState {
    pub(crate) applied: i128,
    pub(crate) total: i128,
    pub(crate) latest_reversal: Option<NaiveDate>,
    latest_active_date: Option<NaiveDate>,
    pub(crate) has_history: bool,
}

fn notes(conn: &Connection, company: i64, invoice: i64) -> Result<Vec<CreditNote>> {
    let mut stmt = conn.prepare(
        "SELECT n.id, v.id, v.date, v.reference, n.amount, n.reason, rv.id, rv.date, n.reversal_reason,
                COALESCE(c.amount, 0)
         FROM invoice_credit_notes n
         JOIN vouchers v ON v.id = n.voucher_id AND v.company_id = n.company_id
         LEFT JOIN vouchers rv ON rv.id = n.reversal_voucher_id AND rv.company_id = n.company_id
         LEFT JOIN invoice_refund_credits c ON c.note_id = n.id
         WHERE n.company_id = ?1 AND n.invoice_id = ?2 ORDER BY v.date, v.id",
    )?;
    let rows = stmt
        .query_map(params![company, invoice], |r| {
            Ok(CreditNote {
                id: r.get(0)?,
                voucher_id: r.get(1)?,
                date: r.get(2)?,
                reference: r.get(3)?,
                amount: r.get(4)?,
                reason: r.get(5)?,
                reversal_voucher_id: r.get(6)?,
                reversal_date: r.get(7)?,
                reversal_reason: r.get(8)?,
                refundable: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn state(
    conn: &Connection,
    company: i64,
    invoice: i64,
    as_of: Option<NaiveDate>,
) -> Result<CreditState> {
    let mut result = CreditState::default();
    for note in notes(conn, company, invoice)? {
        result.has_history = true;
        let date = parse_date(&note.date)?;
        let reversal = note.reversal_date.as_deref().map(parse_date).transpose()?;
        result.latest_reversal = result.latest_reversal.max(reversal);
        if as_of.is_none_or(|end| date <= end)
            && reversal.is_none_or(|date| as_of.is_some_and(|end| date > end))
        {
            result.total += i128::from(note.amount);
            result.applied += i128::from(note.amount) - i128::from(note.refundable);
            result.latest_active_date = result.latest_active_date.max(Some(date));
        }
    }
    let incoming = crate::invoice_credit_allocations::state(conn, company, invoice, as_of)?;
    result.applied += incoming.applied;
    result.latest_reversal = result.latest_reversal.max(incoming.latest_release);
    result.latest_active_date = result.latest_active_date.max(incoming.latest_active_date);
    result.has_history |= incoming.has_history;
    Ok(result)
}

pub(crate) fn is_linked_voucher(conn: &Connection, company: i64, voucher: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM invoice_credit_notes
              WHERE company_id = ?1 AND ?2 IN (voucher_id, reversal_voucher_id)
              UNION ALL SELECT 1 FROM invoice_credit_refunds
              WHERE company_id = ?1 AND ?2 IN (voucher_id, reversal_voucher_id))",
        params![company, voucher],
        |row| row.get(0),
    )?)
}

struct Invoice {
    voucher: i64,
    date: String,
    kind: InvoiceKind,
    party: i64,
    tax_amount: i64,
}

impl Invoice {
    fn reduction_sign(&self) -> i128 {
        match self.kind {
            InvoiceKind::Sale => 1,
            InvoiceKind::Purchase => -1,
        }
    }
}

fn invoice_header(conn: &Connection, company: i64, invoice: i64) -> Result<Invoice> {
    let (voucher, date, kind, party, tax_amount): (i64, String, String, i64, i64) = conn
        .query_row(
            "SELECT v.id, v.date, i.kind, i.party_ledger_id, i.tax_amount
         FROM invoices i JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
         WHERE i.company_id = ?1 AND i.id = ?2",
            params![company, invoice],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Validation("Invoice not found in the selected company".into()))?;
    Ok(Invoice {
        voucher,
        date,
        kind: InvoiceKind::parse(&kind)?,
        party,
        tax_amount,
    })
}

fn components(
    conn: &Connection,
    company: i64,
    id: i64,
    invoice: &Invoice,
) -> Result<Vec<CreditComponent>> {
    let mut original = BTreeMap::<(i64, bool), i128>::new();
    {
        let mut stmt = conn.prepare(
            "SELECT ledger_id, amount FROM postings WHERE voucher_id = ?1 AND ledger_id != ?2",
        )?;
        for row in stmt.query_map(params![invoice.voucher, invoice.party], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (ledger, amount) = row?;
            *original.entry((ledger, false)).or_default() -=
                invoice.reduction_sign() * i128::from(amount);
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT tax_ledger_id, tax_amount FROM invoices WHERE id = ?1 AND tax_amount > 0
             UNION ALL SELECT ledger_id, amount FROM invoice_additional_taxes WHERE invoice_id = ?1",
        )?;
        for row in stmt.query_map([id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (ledger, amount) = row?;
            *original.entry((ledger, true)).or_default() += i128::from(amount);
            *original.entry((ledger, false)).or_default() -= i128::from(amount);
        }
    }
    let mut used = BTreeMap::<(i64, bool), i128>::new();
    {
        let mut stmt = conn.prepare(
            "SELECT l.ledger_id, l.is_tax, l.reduction
             FROM invoice_credit_lines l JOIN invoice_credit_notes n ON n.id = l.note_id
             WHERE n.company_id = ?1 AND n.invoice_id = ?2 AND n.reversal_voucher_id IS NULL",
        )?;
        for row in stmt.query_map(params![company, id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (ledger, tax, reduction) = row?;
            *used.entry((ledger, tax)).or_default() += i128::from(reduction);
        }
    }
    let mut result = Vec::new();
    for ((ledger_id, is_tax), original) in original {
        let used = used.remove(&(ledger_id, is_tax)).unwrap_or_default();
        let remaining = original - used;
        if used.abs() > original.abs() || (used != 0 && used.signum() != original.signum()) {
            return Err(Error::Validation(
                "Credits cannot be reconciled to the invoice postings".into(),
            ));
        }
        if remaining != 0 {
            let ledger = conn.query_row(
                "SELECT name FROM ledgers WHERE company_id = ?1 AND id = ?2",
                params![company, ledger_id],
                |row| row.get(0),
            )?;
            result.push(CreditComponent {
                ledger_id,
                ledger,
                is_tax,
                remaining,
            });
        }
    }
    if !used.is_empty() {
        return Err(Error::Validation(
            "A credit component is missing from the invoice".into(),
        ));
    }
    Ok(result)
}

struct CreditCapacity {
    unpaid: i128,
    remaining_total: i128,
    earliest_refundable_date: Option<NaiveDate>,
}

fn available_for_credit(
    conn: &Connection,
    company: i64,
    id: i64,
    invoice: &Invoice,
    date: NaiveDate,
) -> Result<CreditCapacity> {
    if date < parse_date(&invoice.date)? {
        return Err(Error::Validation(
            "Credit date cannot precede the invoice date".into(),
        ));
    }
    let credits = state(conn, company, id, None)?;
    let advances = crate::advances::invoice_allocation_state(conn, company, id, None)?;
    let mut latest_release = credits.latest_reversal.max(advances.latest_release);
    let mut latest_payment = advances.latest_active_date;
    let mut paid = 0_i128;
    let mut stmt = conn.prepare(
        "SELECT s.amount, v.date, original.date FROM invoice_settlements s
         JOIN vouchers original ON original.id = s.voucher_id AND original.company_id = s.company_id
         LEFT JOIN settlement_reversals r ON r.settlement_voucher_id = s.voucher_id AND r.company_id = s.company_id
         LEFT JOIN vouchers v ON v.id = r.reversal_voucher_id
         WHERE s.company_id = ?1 AND s.invoice_id = ?2",
    )?;
    for row in stmt.query_map(params![company, id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (amount, reversal, payment_date) = row?;
        if let Some(reversal) = reversal {
            latest_release = latest_release.max(Some(parse_date(&reversal)?));
        } else {
            paid += i128::from(amount);
            latest_payment = latest_payment.max(Some(parse_date(&payment_date)?));
        }
    }
    if latest_release.is_some_and(|released| date < released) {
        return Err(Error::Validation(
            "Credit date cannot precede a reversal or advance release on this invoice".into(),
        ));
    }
    let total = invoice_total(conn, id, invoice.tax_amount)?;
    let unpaid = total - credits.applied - advances.applied - paid;
    if unpaid < 0 {
        return Err(Error::Validation(
            "Allocations exceed the original invoice amount".into(),
        ));
    }
    Ok(CreditCapacity {
        unpaid,
        remaining_total: total - credits.total,
        earliest_refundable_date: latest_payment.max(credits.latest_active_date),
    })
}

pub(crate) fn unpaid_for_allocation(
    conn: &Connection,
    company: i64,
    invoice: i64,
    kind: InvoiceKind,
    party: i64,
    date: NaiveDate,
) -> Result<i128> {
    let header = invoice_header(conn, company, invoice)?;
    if header.kind != kind
        || header.party != party
        || crate::invoice_reversals::for_invoice(conn, company, invoice)?.is_some()
    {
        return Err(Error::Validation(
            "Select an active bill of the same type and party as the credit".into(),
        ));
    }
    Ok(available_for_credit(conn, company, invoice, &header, date)?.unpaid)
}

pub(crate) fn insert_invoice_credit(
    tx: &Connection,
    company: i64,
    id: i64,
    date: NaiveDate,
    reference: &str,
    reason: &str,
    lines: &[CreditLineDraft],
    reductions_only: bool,
) -> Result<i64> {
    crate::opening_bills::ensure_trade_invoice(tx, company, id)?;
    if crate::invoice_reversals::for_invoice(tx, company, id)?.is_some() {
        return Err(Error::Validation(
            "A reversed invoice cannot accept a credit or debit note".into(),
        ));
    }
    let invoice = invoice_header(tx, company, id)?;
    let capacity = available_for_credit(tx, company, id, &invoice, date)?;
    let parts = components(tx, company, id, &invoice)?;
    let mut seen = HashSet::new();
    let mut reductions = Vec::new();
    let mut amounts = BTreeMap::<i64, i128>::new();
    let mut total = 0_i128;
    for line in lines {
        let component = parts
            .iter()
            .find(|p| p.ledger_id == line.ledger_id && p.is_tax == line.is_tax)
            .ok_or_else(|| {
                Error::Validation("A selected credit component is unavailable".into())
            })?;
        if line.amount <= 0
            || (reductions_only && component.remaining < 0)
            || i128::from(line.amount) > component.remaining.abs()
            || !seen.insert((line.ledger_id, line.is_tax))
        {
            return Err(Error::Validation(format!(
                "Enter one positive amount within the remaining amount for {}",
                component.ledger
            )));
        }
        let reduction = if component.remaining > 0 {
            line.amount
        } else {
            -line.amount
        };
        reductions.push((line.ledger_id, line.is_tax, reduction));
        *amounts.entry(line.ledger_id).or_default() +=
            invoice.reduction_sign() * i128::from(reduction);
        total += i128::from(reduction);
    }
    if total <= 0 || total > capacity.remaining_total {
        return Err(Error::Validation(
            "The note must be positive and total credits cannot exceed the original invoice amount"
                .into(),
        ));
    }
    let refundable = total - total.min(capacity.unpaid);
    if refundable > 0
        && capacity
            .earliest_refundable_date
            .is_some_and(|day| date < day)
    {
        return Err(Error::Validation("A refundable credit cannot precede an active payment, advance allocation or note on the invoice".into()));
    }
    let total_amount = i64::try_from(total)
        .map_err(|_| Error::Validation("Note amount exceeds the supported range".into()))?;
    if amounts
        .insert(invoice.party, -invoice.reduction_sign() * total)
        .is_some()
    {
        return Err(Error::Validation(
            "The party ledger cannot also be a credit component".into(),
        ));
    }
    let postings = amounts
        .into_iter()
        .filter(|(_, amount)| *amount != 0)
        .map(|(ledger_id, amount)| {
            Ok(Posting {
                ledger_id,
                amount: i64::try_from(amount).map_err(|_| {
                    Error::Validation("Note posting exceeds the supported range".into())
                })?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let kind = match invoice.kind {
        InvoiceKind::Sale => VoucherKind::CreditNote,
        InvoiceKind::Purchase => VoucherKind::DebitNote,
    };
    let voucher = insert_voucher_draft(
        tx,
        company,
        VoucherDraft {
            kind,
            reference,
            date: &date.to_string(),
            narration: reason,
            postings: &postings,
            party_lines: &[],
        },
    )?;
    if tx.execute(
        "INSERT INTO invoice_credit_notes(company_id, invoice_id, voucher_id, reference_key, amount, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(invoice_id, reference_key) DO NOTHING",
        params![company, id, voucher, reference.to_lowercase(), total_amount, reason],
    )? != 1 {
        return Err(Error::Validation("This invoice already has a note with that reference".into()));
    }
    let note = tx.last_insert_rowid();
    if refundable > 0 {
        tx.execute(
            "INSERT INTO invoice_refund_credits(note_id, amount) VALUES (?1, ?2)",
            params![note, refundable as i64],
        )?;
    }
    for (ledger, tax, reduction) in reductions {
        tx.execute(
            "INSERT INTO invoice_credit_lines(note_id, ledger_id, is_tax, reduction)
             VALUES (?1, ?2, ?3, ?4)",
            params![note, ledger, tax, reduction],
        )?;
    }
    Ok(note)
}

impl Store {
    pub fn invoice_credit_notes(&self, company: i64, invoice: i64) -> Result<Vec<CreditNote>> {
        notes(&self.conn, company, invoice)
    }

    pub fn invoice_credit_components(
        &self,
        company: i64,
        invoice: i64,
    ) -> Result<Vec<CreditComponent>> {
        let header = invoice_header(&self.conn, company, invoice)?;
        components(&self.conn, company, invoice, &header)
    }

    pub fn post_invoice_credit(
        &mut self,
        company: i64,
        id: i64,
        date: &str,
        reference: &str,
        reason: &str,
        lines: &[CreditLineDraft],
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Credit or debit note reference")?;
        let reason = required(reason, "Reason for reducing the invoice")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = insert_invoice_credit(&tx, company, id, date, reference, reason, lines, false)?;
        tx.commit()?;
        Ok(note)
    }

    pub fn reverse_invoice_credit(
        &mut self,
        company: i64,
        note: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Reversal reference")?;
        let reason = required(reason, "Reversal reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (original, credit_date): (i64, String) = tx.query_row(
            "SELECT v.id, v.date FROM invoice_credit_notes n JOIN vouchers v ON v.id = n.voucher_id
             WHERE n.company_id = ?1 AND n.id = ?2 AND n.reversal_voucher_id IS NULL",
            params![company, note],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Select an active invoice note".into()))?;
        if date < parse_date(&credit_date)? {
            return Err(Error::Validation(
                "Reversal date cannot precede the note date".into(),
            ));
        }
        crate::invoice_refunds::check_note_reversal(&tx, company, note, date)?;
        let date_text = date.to_string();
        let postings = crate::reversals::counter_postings(&tx, company, original, &date_text)?;
        tx.execute(
            "INSERT INTO vouchers(company_id, date, narration, kind, reference) VALUES (?1, ?2, ?3, 'Journal', ?4)",
            params![company, date_text, format!("Invoice note {note} reversal: {reason}"), reference],
        )?;
        let voucher = tx.last_insert_rowid();
        crate::reversals::write_counter_postings(&tx, company, original, voucher, &postings)?;
        let orders = crate::invoice_returns::reverse_movements(
            &tx,
            company,
            note,
            date,
            &format!("Invoice return {note} reversal: {reason}"),
        )?;
        tx.execute(
            "UPDATE invoice_credit_notes SET reversal_voucher_id = ?1, reversal_reason = ?2
             WHERE company_id = ?3 AND id = ?4",
            params![voucher, reason, company, note],
        )?;
        for order in orders {
            crate::stock_reservations::check_order_coverage(&tx, order)?;
        }
        tx.commit()?;
        Ok(voucher)
    }
}
