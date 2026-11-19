//! Named balances carried by financial notes and applied without a cash posting.
use crate::advances::{check_open_date, read_allocation_state, AllocationState};
use crate::inventory::{BillSettlementTarget, InvoiceKind};
use crate::invoice_credit_allocations::CreditAllocation;
use crate::{params, parse_date, required, Connection, Error, Result, Store, VoucherKind};
use chrono::NaiveDate;
use rusqlite::{OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS bill_note_credits(
            id INTEGER PRIMARY KEY,
            voucher_id INTEGER NOT NULL REFERENCES vouchers(id),
            party_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            reference TEXT NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0),
            due_date TEXT,
            credit_days INTEGER CHECK(credit_days >= 0),
            CHECK(due_date IS NULL OR credit_days IS NULL),
            UNIQUE(voucher_id, reference)
        );
        CREATE TABLE IF NOT EXISTS bill_note_credit_applications(
            id INTEGER PRIMARY KEY,
            credit_id INTEGER NOT NULL REFERENCES bill_note_credits(id),
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            split_id INTEGER,
            date TEXT NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0),
            released_on TEXT,
            release_reason TEXT,
            CHECK((released_on IS NULL) = (release_reason IS NULL)),
            FOREIGN KEY(split_id, invoice_id) REFERENCES invoice_bill_splits(id, invoice_id)
        );
        CREATE INDEX IF NOT EXISTS bill_note_credit_applications_credit ON bill_note_credit_applications(credit_id);
        CREATE INDEX IF NOT EXISTS bill_note_credit_applications_invoice ON bill_note_credit_applications(invoice_id);
        CREATE INDEX IF NOT EXISTS bill_note_credit_applications_split ON bill_note_credit_applications(split_id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct BillNoteCredit {
    pub id: i64,
    pub voucher_id: i64,
    pub kind: InvoiceKind,
    pub party_ledger_id: i64,
    pub party: String,
    pub reference: String,
    pub voucher_reference: String,
    pub date: String,
    pub due_date: Option<String>,
    pub credit_days: Option<u32>,
    pub amount: i64,
    pub available: i128,
    pub reversal_date: Option<String>,
    pub(crate) latest_release: Option<NaiveDate>,
}

fn allocation_state(
    conn: &Connection,
    credit: i64,
    through: Option<NaiveDate>,
) -> Result<AllocationState> {
    let mut stmt = conn.prepare_cached(
        "SELECT date, amount, released_on FROM bill_note_credit_applications WHERE credit_id = ?1
         UNION ALL SELECT date, amount, reversal_date FROM bill_note_refund_history WHERE credit_id = ?1",
    )?;
    let rows = stmt.query([credit])?;
    read_allocation_state(rows, through)
}

pub(crate) fn read_credits(
    conn: &Connection,
    company: i64,
    only: Option<i64>,
    through: Option<NaiveDate>,
) -> Result<Vec<BillNoteCredit>> {
    let mut stmt = conn.prepare_cached(
        "SELECT c.id, v.id, v.kind, l.id, l.name, c.reference, v.reference, v.date, c.due_date,
                c.credit_days, c.amount, rv.date
         FROM bill_note_credits c JOIN vouchers v ON v.id = c.voucher_id
         JOIN ledgers l ON l.id = c.party_ledger_id AND l.company_id = v.company_id
         LEFT JOIN voucher_reversals r ON r.company_id = v.company_id AND r.original_voucher_id = v.id
         LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
         WHERE v.company_id = ?1 AND (?2 IS NULL OR c.id = ?2) AND (?3 IS NULL OR v.date <= ?3)
         ORDER BY v.date, v.id, c.id",
    )?;
    let mut rows = stmt.query(params![company, only, through.map(|date| date.to_string())])?;
    let mut credits = Vec::new();
    while let Some(row) = rows.next()? {
        let id = row.get(0)?;
        let kind = match VoucherKind::parse(&row.get::<_, String>(2)?)? {
            VoucherKind::CreditNote => InvoiceKind::Sale,
            VoucherKind::DebitNote => InvoiceKind::Purchase,
            _ => {
                return Err(Error::Validation(
                    "Credit voucher type is inconsistent".into(),
                ))
            }
        };
        let amount = row.get::<_, i64>(10)?;
        let reversal_date = row.get::<_, Option<String>>(11)?;
        let reversed = reversal_date
            .as_deref()
            .map(parse_date)
            .transpose()?
            .is_some_and(|date| through.is_none_or(|end| date <= end));
        let used = allocation_state(conn, id, through)?;
        let remaining = i128::from(amount) - used.applied;
        if remaining < 0 || (reversed && used.applied != 0) {
            return Err(Error::Validation(
                "Applications do not reconcile to the note credit".into(),
            ));
        }
        credits.push(BillNoteCredit {
            id,
            kind,
            amount,
            reversal_date,
            voucher_id: row.get(1)?,
            party_ledger_id: row.get(3)?,
            party: row.get(4)?,
            reference: row.get(5)?,
            voucher_reference: row.get(6)?,
            date: row.get(7)?,
            due_date: row.get(8)?,
            credit_days: row.get(9)?,
            available: if reversed { 0 } else { remaining },
            latest_release: used.latest_release,
        });
    }
    Ok(credits)
}

pub(crate) fn insert_application(
    conn: &Connection,
    company: i64,
    credit_id: i64,
    target: BillSettlementTarget,
    date: NaiveDate,
    amount: i64,
) -> Result<i64> {
    check_open_date(conn, company, date)?;
    let credit = read_credits(conn, company, Some(credit_id), None)?
        .pop()
        .ok_or_else(|| Error::Validation("Select a named credit in this company".into()))?;
    if date < parse_date(&credit.date)?
        || credit.latest_release.is_some_and(|release| date < release)
    {
        return Err(Error::Validation(
            "Application cannot precede the credit or its latest release".into(),
        ));
    }
    if amount <= 0 || i128::from(amount) > credit.available {
        return Err(Error::Validation(
            "Enter a positive amount within the available credit".into(),
        ));
    }
    let unpaid = crate::invoice_credits::unpaid_for_allocation(
        conn,
        company,
        target.invoice_id,
        credit.kind,
        credit.party_ledger_id,
        date,
    )?;
    let unpaid = crate::bill_splits::target(conn, target)?
        .map_or(unpaid, |bill| unpaid.min(bill.outstanding()));
    if i128::from(amount) > unpaid {
        return Err(Error::Validation(
            "Credit application exceeds the outstanding bill balance".into(),
        ));
    }
    conn.execute(
        "INSERT INTO bill_note_credit_applications(credit_id, invoice_id, split_id, date, amount) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![credit_id, target.invoice_id, target.split_id, date.to_string(), amount],
    )?;
    Ok(conn.last_insert_rowid())
}

pub(crate) fn check_reversal(
    conn: &Connection,
    company: i64,
    voucher: i64,
    date: NaiveDate,
) -> Result<()> {
    let conflict: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bill_note_credit_applications a JOIN bill_note_credits c ON c.id = a.credit_id
         JOIN vouchers v ON v.id = c.voucher_id WHERE c.voucher_id = ?1 AND v.company_id = ?2
         AND (a.released_on IS NULL OR a.released_on > ?3)
         UNION ALL SELECT 1 FROM bill_note_refund_history f JOIN bill_note_credits c ON c.id = f.credit_id
         WHERE c.voucher_id = ?1 AND f.company_id = ?2 AND (f.reversal_date IS NULL OR f.reversal_date > ?3))",
        params![voucher, company, date.to_string()], |row| row.get(0),
    )?;
    if conflict {
        return Err(Error::Validation(
            "Release all applications and reverse any refunds before reversing the note".into(),
        ));
    }
    Ok(())
}

impl Store {
    pub fn bill_note_credits(
        &self,
        company: i64,
        as_of: Option<&str>,
    ) -> Result<Vec<BillNoteCredit>> {
        read_credits(
            &self.conn,
            company,
            None,
            as_of.map(parse_date).transpose()?,
        )
    }

    pub fn bill_note_credit_applications(
        &self,
        company: i64,
        credit: i64,
    ) -> Result<Vec<CreditAllocation>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.id, iv.reference, COALESCE(b.reference, i.bill_reference, iv.reference),
                    a.date, a.amount, a.released_on, a.release_reason
             FROM bill_note_credit_applications a JOIN bill_note_credits c ON c.id = a.credit_id
             JOIN vouchers v ON v.id = c.voucher_id
             JOIN invoices i ON i.id = a.invoice_id AND i.company_id = v.company_id
             JOIN vouchers iv ON iv.id = i.voucher_id LEFT JOIN invoice_bill_splits b ON b.id = a.split_id
             WHERE v.company_id = ?1 AND c.id = ?2 ORDER BY a.date, a.id",
        )?;
        let rows = stmt.query_map(params![company, credit], |row| {
            Ok(CreditAllocation {
                id: row.get(0)?,
                invoice_reference: row.get(1)?,
                bill_reference: row.get(2)?,
                date: row.get(3)?,
                amount: row.get(4)?,
                released_on: row.get(5)?,
                release_reason: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn apply_bill_note_credit(
        &mut self,
        company: i64,
        credit: i64,
        target: BillSettlementTarget,
        date: &str,
        amount: i64,
    ) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = insert_application(&tx, company, credit, target, parse_date(date)?, amount)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn release_bill_note_credit(
        &mut self,
        company: i64,
        application: i64,
        date: &str,
        reason: &str,
    ) -> Result<()> {
        let date = parse_date(date)?;
        let reason = required(reason, "Credit release reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_open_date(&tx, company, date)?;
        let original = tx.query_row(
            "SELECT a.date FROM bill_note_credit_applications a JOIN bill_note_credits c ON c.id = a.credit_id
             JOIN vouchers v ON v.id = c.voucher_id WHERE v.company_id = ?1 AND a.id = ?2 AND a.released_on IS NULL",
            params![company, application], |row| row.get::<_, String>(0),
        ).optional()?.ok_or_else(|| Error::Validation("Select an active credit application".into()))?;
        if date < parse_date(&original)? {
            return Err(Error::Validation(
                "Credit release date cannot precede the application".into(),
            ));
        }
        tx.execute(
            "UPDATE bill_note_credit_applications SET released_on = ?1, release_reason = ?2 WHERE id = ?3",
            params![date.to_string(), reason, application],
        )?;
        tx.commit()?;
        Ok(())
    }
}
