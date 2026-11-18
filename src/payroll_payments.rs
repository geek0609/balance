//! Dated salary payments against individual payroll accruals.
use crate::inventory::ledger_category;
use crate::reversals::{counter_postings, write_counter_postings};
use crate::{parse_date, required, Category, Error, Result, Store, VoucherKind};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE payroll_accruals ADD COLUMN payable_ledger_id INTEGER REFERENCES ledgers(id);
        UPDATE payroll_accruals SET payable_ledger_id = (
            SELECT e.payable_ledger_id FROM payroll_employees e WHERE e.id = payroll_accruals.employee_id
        );
        CREATE TABLE payroll_payments(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            accrual_id INTEGER NOT NULL REFERENCES payroll_accruals(id),
            voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id),
            bank_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            reference_key TEXT NOT NULL,
            reversal_voucher_id INTEGER UNIQUE REFERENCES vouchers(id),
            reversal_reason TEXT,
            CHECK((reversal_voucher_id IS NULL) = (reversal_reason IS NULL)),
            UNIQUE(accrual_id, reference_key)
        );
        CREATE INDEX payroll_payments_accrual ON payroll_payments(company_id, accrual_id, id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PayrollPaymentDraft {
    pub bank_ledger_id: i64,
    pub amount: i64,
    pub date: String,
    pub reference: String,
    pub narration: String,
}

pub type PayrollPaymentReversal = crate::payroll::PayrollReversal;

#[derive(Debug, Clone)]
pub struct PayrollPayment {
    pub id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub bank: String,
    pub amount: i64,
    pub reversal: Option<PayrollPaymentReversal>,
}

#[derive(Debug, Default)]
pub(crate) struct PaymentState {
    pub paid: i128,
    pub latest_reversal: Option<NaiveDate>,
}

pub(crate) fn payment_state(
    conn: &Connection,
    company: i64,
    accrual: i64,
    end: Option<NaiveDate>,
) -> Result<PaymentState> {
    let mut stmt = conn.prepare(
        "SELECT v.date, p.amount, rv.date FROM payroll_payments p
         JOIN vouchers v ON v.id = p.voucher_id AND v.company_id = p.company_id
         LEFT JOIN vouchers rv ON rv.id = p.reversal_voucher_id AND rv.company_id = p.company_id
         WHERE p.company_id = ?1 AND p.accrual_id = ?2",
    )?;
    let mut rows = stmt.query(params![company, accrual])?;
    let mut state = PaymentState::default();
    while let Some(row) = rows.next()? {
        let date = parse_date(&row.get::<_, String>(0)?)?;
        let amount = i128::from(row.get::<_, i64>(1)?);
        let reversal = row
            .get::<_, Option<String>>(2)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        state.latest_reversal = state.latest_reversal.max(reversal);
        let active = match end {
            Some(end) => date <= end && reversal.is_none_or(|reversed| end < reversed),
            None => reversal.is_none(),
        };
        if active {
            state.paid += amount;
        }
    }
    Ok(state)
}

impl Store {
    pub fn pay_payroll_accrual(
        &mut self,
        company: i64,
        accrual: i64,
        draft: &PayrollPaymentDraft,
    ) -> Result<i64> {
        let date = parse_date(&draft.date)?;
        let reference = required(&draft.reference, "Payment reference")?;
        if draft.amount <= 0 {
            return Err(Error::Validation("Enter a positive payment amount".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (accrued_on, payable, net, reversed): (String, i64, i64, bool) = tx
            .query_row(
                "SELECT v.date, a.payable_ledger_id, a.net, a.reversal_voucher_id IS NOT NULL FROM payroll_accruals a
             JOIN vouchers v ON v.id = a.voucher_id AND v.company_id = a.company_id
             WHERE a.company_id = ?1 AND a.id = ?2",
                params![company, accrual],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Payroll accrual not found in this company".into()))?;
        if reversed {
            return Err(Error::Validation(
                "A reversed accrual cannot accept a payment".into(),
            ));
        }
        if date < parse_date(&accrued_on)? {
            return Err(Error::Validation(
                "Payment date cannot precede the accrual date".into(),
            ));
        }
        if ledger_category(&tx, company, draft.bank_ledger_id)? != Category::Asset
            || ledger_category(&tx, company, payable)? != Category::Liability
        {
            return Err(Error::Validation(
                "Choose an asset ledger for the payment".into(),
            ));
        }
        let state = payment_state(&tx, company, accrual, None)?;
        if state
            .latest_reversal
            .is_some_and(|reversed| date < reversed)
        {
            return Err(Error::Validation(
                "Payment date cannot precede a reversal on the accrual".into(),
            ));
        }
        if i128::from(draft.amount) > i128::from(net) - state.paid {
            return Err(Error::Validation(
                "Payment exceeds the outstanding salary".into(),
            ));
        }
        let date = date.format("%Y-%m-%d").to_string();
        tx.execute(
            "INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![company, VoucherKind::Payment.as_str(), reference, date, draft.narration.trim()],
        )?;
        let voucher = tx.last_insert_rowid();
        for (ledger, amount) in [
            (payable, draft.amount),
            (draft.bank_ledger_id, -draft.amount),
        ] {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![voucher, ledger, amount],
            )?;
        }
        let inserted = tx.execute(
            "INSERT INTO payroll_payments(company_id, accrual_id, voucher_id, bank_ledger_id, amount, reference_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(accrual_id, reference_key) DO NOTHING",
            params![company, accrual, voucher, draft.bank_ledger_id, draft.amount, reference.to_lowercase()],
        )?;
        if inserted != 1 {
            return Err(Error::Validation(
                "This salary payment reference already exists for the accrual".into(),
            ));
        }
        let payment = tx.last_insert_rowid();
        tx.commit()?;
        Ok(payment)
    }

    pub fn reverse_payroll_payment(
        &mut self,
        company: i64,
        payment: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Reversal reference")?;
        let reason = required(reason, "Reason for reversing the salary payment")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (voucher, paid_on, reversal): (i64, String, Option<i64>) = tx.query_row(
            "SELECT v.id, v.date, p.reversal_voucher_id FROM payroll_payments p JOIN vouchers v ON v.id = p.voucher_id
             WHERE p.company_id = ?1 AND p.id = ?2",
            params![company, payment], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Salary payment not found in this company".into()))?;
        if reversal.is_some() {
            return Err(Error::Validation(
                "This salary payment has already been reversed".into(),
            ));
        }
        if date < parse_date(&paid_on)? {
            return Err(Error::Validation(
                "Reversal date cannot precede the payment".into(),
            ));
        }
        let date = date.format("%Y-%m-%d").to_string();
        let postings = counter_postings(&tx, company, voucher, &date)?;
        tx.execute(
            "INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![company, VoucherKind::Receipt.as_str(), reference, date, format!("Salary payment reversal {payment}: {reason}")],
        )?;
        let reversed = tx.last_insert_rowid();
        write_counter_postings(&tx, company, voucher, reversed, &postings)?;
        tx.execute(
            "UPDATE payroll_payments SET reversal_voucher_id = ?1, reversal_reason = ?2
                    WHERE company_id = ?3 AND id = ?4",
            params![reversed, reason, company, payment],
        )?;
        tx.commit()?;
        Ok(reversed)
    }

    pub fn payroll_payments(
        &self,
        company: i64,
        accrual: i64,
        end: Option<&str>,
    ) -> Result<Vec<PayrollPayment>> {
        let end = end.map(parse_date).transpose()?;
        let mut stmt = self.conn.prepare(
            "SELECT p.id, v.id, v.date, v.reference, l.name, p.amount, rv.id, rv.date, rv.reference, p.reversal_reason
             FROM payroll_payments p JOIN vouchers v ON v.id = p.voucher_id AND v.company_id = p.company_id
             JOIN ledgers l ON l.id = p.bank_ledger_id AND l.company_id = p.company_id
             LEFT JOIN vouchers rv ON rv.id = p.reversal_voucher_id AND rv.company_id = p.company_id
             WHERE p.company_id = ?1 AND p.accrual_id = ?2 ORDER BY v.date, p.id",
        )?;
        let mut rows = stmt.query(params![company, accrual])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let date: String = row.get(2)?;
            let parsed = parse_date(&date)?;
            if end.is_some_and(|end| parsed > end) {
                continue;
            }
            let reversal = if let Some(voucher_id) = row.get::<_, Option<i64>>(6)? {
                let date: String = row.get(7)?;
                let parsed = parse_date(&date)?;
                if end.is_none_or(|end| parsed <= end) {
                    Some(PayrollPaymentReversal {
                        voucher_id,
                        date,
                        reference: row.get(8)?,
                        reason: row.get(9)?,
                    })
                } else {
                    None
                }
            } else {
                None
            };
            result.push(PayrollPayment {
                id: row.get(0)?,
                voucher_id: row.get(1)?,
                date,
                reference: row.get(3)?,
                bank: row.get(4)?,
                amount: row.get(5)?,
                reversal,
            });
        }
        Ok(result)
    }
}
