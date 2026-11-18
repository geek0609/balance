//! On-account receipts and payments for customers and suppliers.
use crate::inventory::{ledger_category, InvoiceKind};
use crate::reversals::{counter_postings, write_counter_postings};
use crate::{parse_date, required, Category, Error, Result, Store, VoucherKind};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE party_advances(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            kind TEXT NOT NULL CHECK(kind IN ('Sale', 'Purchase')),
            party_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            bank_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            reference_key TEXT NOT NULL,
            reversal_voucher_id INTEGER UNIQUE REFERENCES vouchers(id),
            reversal_reason TEXT,
            CHECK((reversal_voucher_id IS NULL) = (reversal_reason IS NULL)),
            CHECK(party_ledger_id != bank_ledger_id),
            UNIQUE(company_id, kind, party_ledger_id, reference_key)
        );
        CREATE INDEX party_advances_company ON party_advances(company_id, id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct AdvanceDraft {
    pub kind: InvoiceKind,
    pub party_ledger_id: i64,
    pub bank_ledger_id: i64,
    pub date: String,
    pub reference: String,
    pub amount: i64,
    pub narration: String,
}

#[derive(Debug, Clone)]
pub struct AdvanceReversal {
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PartyAdvance {
    pub id: i64,
    pub kind: InvoiceKind,
    pub party_ledger_id: i64,
    pub party: String,
    pub bank: String,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub narration: String,
    pub amount: i64,
    pub reversal: Option<AdvanceReversal>,
}

impl Store {
    pub fn post_party_advance(&mut self, company: i64, draft: &AdvanceDraft) -> Result<i64> {
        let date = parse_date(&draft.date)?.format("%Y-%m-%d").to_string();
        let reference = required(&draft.reference, "Advance reference")?;
        if draft.amount <= 0 {
            return Err(Error::Validation("Enter a positive advance amount".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !matches!(
            ledger_category(&tx, company, draft.party_ledger_id)?,
            Category::Asset | Category::Liability
        ) {
            return Err(Error::Validation(
                "The party ledger must be an asset or liability".into(),
            ));
        }
        if ledger_category(&tx, company, draft.bank_ledger_id)? != Category::Asset {
            return Err(Error::Validation(
                "The cash or bank ledger must be an asset".into(),
            ));
        }
        if draft.party_ledger_id == draft.bank_ledger_id {
            return Err(Error::Validation(
                "Choose a different ledger for cash or bank".into(),
            ));
        }
        let key = reference.to_lowercase();
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM party_advances
                           WHERE company_id = ?1 AND kind = ?2 AND party_ledger_id = ?3 AND reference_key = ?4)",
            params![company, draft.kind.as_str(), draft.party_ledger_id, key], |row| row.get(0),
        )?;
        if duplicate {
            return Err(Error::Validation(
                "An advance with this party, type and reference already exists".into(),
            ));
        }
        let (voucher_kind, cash) = match draft.kind {
            InvoiceKind::Sale => (VoucherKind::Receipt, draft.amount),
            InvoiceKind::Purchase => (VoucherKind::Payment, -draft.amount),
        };
        tx.execute(
            "INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![company, voucher_kind.as_str(), reference, date, draft.narration.trim()],
        )?;
        let voucher = tx.last_insert_rowid();
        for (ledger, amount) in [(draft.bank_ledger_id, cash), (draft.party_ledger_id, -cash)] {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![voucher, ledger, amount],
            )?;
        }
        tx.execute(
            "INSERT INTO party_advances(company_id, kind, party_ledger_id, bank_ledger_id, voucher_id, amount, reference_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![company, draft.kind.as_str(), draft.party_ledger_id, draft.bank_ledger_id, voucher, draft.amount, key],
        )?;
        let advance = tx.last_insert_rowid();
        tx.commit()?;
        Ok(advance)
    }

    pub fn reverse_party_advance(
        &mut self,
        company: i64,
        advance: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Reversal reference")?;
        let reason = required(reason, "Reason for reversing the advance")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (original, original_date, kind, prior): (i64, String, String, Option<i64>) = tx
            .query_row(
                "SELECT v.id, v.date, a.kind, a.reversal_voucher_id FROM party_advances a
             JOIN vouchers v ON v.id = a.voucher_id AND v.company_id = a.company_id
             WHERE a.company_id = ?1 AND a.id = ?2",
                params![company, advance],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Advance not found in this company".into()))?;
        if prior.is_some() {
            return Err(Error::Validation(
                "This advance has already been reversed".into(),
            ));
        }
        if date < parse_date(&original_date)? {
            return Err(Error::Validation(
                "Reversal date cannot precede the advance date".into(),
            ));
        }
        let kind = match InvoiceKind::parse(&kind)? {
            InvoiceKind::Sale => VoucherKind::Payment,
            InvoiceKind::Purchase => VoucherKind::Receipt,
        };
        let date = date.format("%Y-%m-%d").to_string();
        let postings = counter_postings(&tx, company, original, &date)?;
        tx.execute(
            "INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![company, kind.as_str(), reference, date, format!("Advance {advance} reversal: {reason}")],
        )?;
        let voucher = tx.last_insert_rowid();
        write_counter_postings(&tx, company, original, voucher, &postings)?;
        tx.execute(
            "UPDATE party_advances SET reversal_voucher_id = ?1, reversal_reason = ?2 WHERE id = ?3 AND company_id = ?4",
            params![voucher, reason, advance, company],
        )?;
        tx.commit()?;
        Ok(voucher)
    }

    /// Show all advances and their reversals, or advance activity through a date.
    pub fn party_advances(&self, company: i64, through: Option<&str>) -> Result<Vec<PartyAdvance>> {
        let through = through.map(parse_date).transpose()?;
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.kind, a.party_ledger_id, party.name, bank.name, v.id, v.date, v.reference,
                    v.narration, a.amount, rv.id, rv.date, rv.reference, a.reversal_reason
             FROM party_advances a JOIN vouchers v ON v.id = a.voucher_id AND v.company_id = a.company_id
             JOIN ledgers party ON party.id = a.party_ledger_id AND party.company_id = a.company_id
             JOIN ledgers bank ON bank.id = a.bank_ledger_id AND bank.company_id = a.company_id
             LEFT JOIN vouchers rv ON rv.id = a.reversal_voucher_id AND rv.company_id = a.company_id
             WHERE a.company_id = ?1",
        )?;
        let mut rows = stmt.query([company])?;
        let mut advances = Vec::new();
        while let Some(row) = rows.next()? {
            let date: String = row.get(6)?;
            let original_date = parse_date(&date)?;
            if through.is_some_and(|limit| original_date > limit) {
                continue;
            }
            let reversal = if let Some(voucher_id) = row.get(10)? {
                let date: String = row.get(11)?;
                let parsed = parse_date(&date)?;
                if through.is_none_or(|limit| parsed <= limit) {
                    Some(AdvanceReversal {
                        voucher_id,
                        date,
                        reference: row.get(12)?,
                        reason: row.get(13)?,
                    })
                } else {
                    None
                }
            } else {
                None
            };
            advances.push((
                original_date,
                PartyAdvance {
                    id: row.get(0)?,
                    kind: InvoiceKind::parse(&row.get::<_, String>(1)?)?,
                    party_ledger_id: row.get(2)?,
                    party: row.get(3)?,
                    bank: row.get(4)?,
                    voucher_id: row.get(5)?,
                    date,
                    reference: row.get(7)?,
                    narration: row.get(8)?,
                    amount: row.get(9)?,
                    reversal,
                },
            ));
        }
        advances.sort_by(|(a, row_a), (b, row_b)| b.cmp(a).then_with(|| row_b.id.cmp(&row_a.id)));
        Ok(advances.into_iter().map(|(_, advance)| advance).collect())
    }
}
