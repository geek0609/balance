//! Dated counter-entries for ordinary accounting vouchers.
use crate::{
    check_ledgers, parse_date, required, validate_voucher, Error, Posting, Result, Store,
    VoucherKind,
};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone)]
pub struct VoucherReversal {
    pub original_voucher_id: i64,
    pub reversal_voucher_id: i64,
    pub date: String,
    pub reason: String,
}

impl Store {
    pub fn voucher_reversal(&self, company: i64, voucher: i64) -> Result<Option<VoucherReversal>> {
        Ok(self.conn.query_row("SELECT r.original_voucher_id, r.reversal_voucher_id, v.date, r.reason FROM voucher_reversals r JOIN vouchers v ON v.id = r.reversal_voucher_id AND v.company_id = r.company_id WHERE r.company_id = ?1 AND (r.original_voucher_id = ?2 OR r.reversal_voucher_id = ?2)", params![company, voucher], |r| Ok(VoucherReversal { original_voucher_id: r.get(0)?, reversal_voucher_id: r.get(1)?, date: r.get(2)?, reason: r.get(3)? })).optional()?)
    }

    /// Post a separate counter-entry. The original and its counter-entry cannot be edited.
    pub fn reverse_voucher(
        &mut self,
        company: i64,
        original: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Reversal reference")?;
        let reason = required(reason, "Reversal reason")?;
        let tx = self.conn.transaction()?;
        let original_date: String = tx
            .query_row(
                "SELECT date FROM vouchers WHERE company_id = ?1 AND id = ?2",
                params![company, original],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Voucher not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        if date < parse_date(&original_date)? {
            return Err(Error::Validation(
                "Reversal date must not precede the original voucher".into(),
            ));
        }
        let linked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM invoices WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM invoice_settlements WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM settlement_reversals WHERE company_id = ?1 AND reversal_voucher_id = ?2 UNION ALL SELECT 1 FROM opening_balances WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM voucher_reversals WHERE company_id = ?1 AND (original_voucher_id = ?2 OR reversal_voucher_id = ?2))", params![company, original], |r| r.get(0))?;
        if linked {
            return Err(Error::Validation(
                "Linked or already reversed vouchers cannot be reversed here".into(),
            ));
        }
        let mut stmt =
            tx.prepare("SELECT ledger_id, amount FROM postings WHERE voucher_id = ?1 ORDER BY id")?;
        let postings = stmt
            .query_map([original], |r| {
                Ok(Posting {
                    ledger_id: r.get(0)?,
                    amount: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let reversed = postings
            .into_iter()
            .map(|p| {
                Ok(Posting {
                    ledger_id: p.ledger_id,
                    amount: p.amount.checked_neg().ok_or_else(|| {
                        Error::Validation("Posting amount cannot be reversed".into())
                    })?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let date = date.format("%Y-%m-%d").to_string();
        validate_voucher(&date, &reversed)?;
        check_ledgers(&tx, company, &reversed)?;
        crate::opening::check_no_control_postings(&tx, company, &reversed)?;
        tx.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company, VoucherKind::Journal.as_str(), reference, date, format!("Reversal of voucher {original}: {reason}")])?;
        let reversal = tx.last_insert_rowid();
        for posting in reversed {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![reversal, posting.ledger_id, posting.amount],
            )?;
        }
        tx.execute("INSERT INTO voucher_reversals(company_id, original_voucher_id, reversal_voucher_id, reason) VALUES (?1, ?2, ?3, ?4)", params![company, original, reversal, reason])?;
        tx.commit()?;
        Ok(reversal)
    }
}
