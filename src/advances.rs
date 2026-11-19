//! On-account receipts and payments for customers and suppliers.
use crate::inventory::{invoice_total, ledger_category, BillSettlementTarget, InvoiceKind};
use crate::reversals::{counter_postings, write_counter_postings};
use crate::{parse_date, required, Category, Error, Result, Store, VoucherKind};
use chrono::NaiveDate;
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
    pub includes_settlements: bool,
    pub opening_original_date: Option<String>,
    pub applied: i128,
    pub available: i128,
    pub source_bill: Option<AdvanceSourceBill>,
    pub reversal: Option<AdvanceReversal>,
}

/// Original bill reference, classification and terms for an imported advance.
#[derive(Debug, Clone)]
pub struct AdvanceSourceBill {
    pub kind: String,
    pub reference: Option<String>,
    pub due_date: Option<String>,
    pub credit_days: Option<u32>,
}

impl AdvanceSourceBill {
    pub(crate) fn named_reference(&self) -> Option<&str> {
        self.reference
            .as_deref()
            .filter(|_| matches!(self.kind.as_str(), "Advance" | "New Ref"))
    }
}

pub(crate) fn migrate_source_bill(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE party_advances ADD COLUMN source_bill_type TEXT CHECK(source_bill_type IN ('Advance', 'On Account'));
         ALTER TABLE party_advances ADD COLUMN source_bill_reference TEXT;",
    )?;
    Ok(())
}

pub(crate) fn migrate_combined_reversal_guard(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS combined_advance_reversal
         BEFORE UPDATE OF reversal_voucher_id ON party_advances
         WHEN NEW.reversal_voucher_id IS NOT NULL
           AND EXISTS (
               SELECT 1 FROM invoice_settlements
               WHERE company_id = NEW.company_id AND voucher_id = NEW.voucher_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM settlement_reversals
               WHERE company_id = NEW.company_id AND settlement_voucher_id = NEW.voucher_id
                   AND reversal_voucher_id = NEW.reversal_voucher_id
           )
         BEGIN
             SELECT RAISE(ABORT, 'Reverse the entire combined receipt or payment');
         END;",
    )?;
    Ok(())
}

pub(crate) fn migrate_source_bill_terms(conn: &Connection) -> Result<()> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('party_advances') WHERE name = 'source_bill_credit_days')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        conn.execute_batch(
            "ALTER TABLE party_advances RENAME COLUMN source_bill_type TO previous_source_bill_type;
             ALTER TABLE party_advances ADD COLUMN source_bill_type TEXT
                 CHECK(source_bill_type IN ('Advance', 'On Account', 'New Ref'));
             ALTER TABLE party_advances ADD COLUMN source_bill_due_date TEXT;
             ALTER TABLE party_advances ADD COLUMN source_bill_credit_days INTEGER
                 CHECK(source_bill_credit_days IS NULL OR
                     (source_bill_credit_days BETWEEN 0 AND 4294967295 AND source_bill_due_date IS NULL));
             UPDATE party_advances SET source_bill_type = previous_source_bill_type
                 WHERE previous_source_bill_type IS NOT NULL;
             ALTER TABLE party_advances DROP COLUMN previous_source_bill_type;",
        )?;
    }
    Ok(())
}

pub(crate) fn migrate_opening_advances(conn: &Connection) -> Result<()> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('party_advances') WHERE name = 'opening_original_date')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        conn.execute_batch("ALTER TABLE party_advances ADD COLUMN opening_original_date TEXT;")?;
    }
    Ok(())
}

pub(crate) fn insert_party_advance(
    conn: &Connection,
    company: i64,
    draft: &AdvanceDraft,
    source_bill: Option<&AdvanceSourceBill>,
    settled_amount: i64,
) -> Result<(i64, i64)> {
    insert_advance(conn, company, draft, source_bill, settled_amount, None)
}

pub(crate) fn insert_opening_party_advance(
    conn: &Connection,
    company: i64,
    draft: &AdvanceDraft,
    source_bill: &AdvanceSourceBill,
    original_date: NaiveDate,
) -> Result<(i64, i64)> {
    insert_advance(
        conn,
        company,
        draft,
        Some(source_bill),
        0,
        Some(original_date),
    )
}

fn insert_advance(
    conn: &Connection,
    company: i64,
    draft: &AdvanceDraft,
    source_bill: Option<&AdvanceSourceBill>,
    settled_amount: i64,
    opening_original_date: Option<NaiveDate>,
) -> Result<(i64, i64)> {
    let date = parse_date(&draft.date)?;
    let reference = required(&draft.reference, "Advance reference")?;
    if draft.amount <= 0 {
        return Err(Error::Validation("Enter a positive advance amount".into()));
    }
    if !matches!(
        ledger_category(conn, company, draft.party_ledger_id)?,
        Category::Asset | Category::Liability
    ) {
        return Err(Error::Validation(
            "The party ledger must be an asset or liability".into(),
        ));
    }
    if let Some(original) = opening_original_date {
        if original > date
            || crate::opening::control_id(conn, company)? != Some(draft.bank_ledger_id)
        {
            return Err(Error::Validation("The opening advance needs an original date on or before its opening date and the opening difference ledger".into()));
        }
    } else if ledger_category(conn, company, draft.bank_ledger_id)? != Category::Asset {
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
    let duplicate: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM party_advances
                       WHERE company_id = ?1 AND kind = ?2 AND party_ledger_id = ?3 AND reference_key = ?4)",
        params![company, draft.kind.as_str(), draft.party_ledger_id, key], |row| row.get(0),
    )?;
    if duplicate {
        return Err(Error::Validation(
            "An advance with this party, type and reference already exists".into(),
        ));
    }
    let total = draft
        .amount
        .checked_add(settled_amount)
        .filter(|_| settled_amount >= 0)
        .ok_or_else(|| Error::Validation("Invalid receipt or payment total".into()))?;
    let (mut voucher_kind, counterpart) = match draft.kind {
        InvoiceKind::Sale => (VoucherKind::Receipt, total),
        InvoiceKind::Purchase => (VoucherKind::Payment, -total),
    };
    if opening_original_date.is_some() {
        voucher_kind = VoucherKind::Journal;
    }
    conn.execute(
        "INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![company, voucher_kind.as_str(), reference, date.to_string(), draft.narration.trim()],
    )?;
    let voucher = conn.last_insert_rowid();
    for (ledger, amount) in [
        (draft.bank_ledger_id, counterpart),
        (draft.party_ledger_id, -counterpart),
    ] {
        conn.execute(
            "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
            params![voucher, ledger, amount],
        )?;
    }
    conn.execute(
        "INSERT INTO party_advances(company_id, kind, party_ledger_id, bank_ledger_id, voucher_id, amount, reference_key,
                                    source_bill_type, source_bill_reference, source_bill_due_date, source_bill_credit_days, opening_original_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            company,
            draft.kind.as_str(),
            draft.party_ledger_id,
            draft.bank_ledger_id,
            voucher,
            draft.amount,
            key,
            source_bill.map(|bill| bill.kind.as_str()),
            source_bill.and_then(|bill| bill.reference.as_deref()),
            source_bill.and_then(|bill| bill.due_date.as_deref()),
            source_bill.and_then(|bill| bill.credit_days),
            opening_original_date.map(|date| date.to_string()),
        ],
    )?;
    Ok((conn.last_insert_rowid(), voucher))
}

impl Store {
    pub fn post_party_advance(&mut self, company: i64, draft: &AdvanceDraft) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (advance, _) = insert_party_advance(&tx, company, draft, None, 0)?;
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
        let (original, original_date, kind, prior, opening): (i64, String, String, Option<i64>, bool) = tx
            .query_row(
                "SELECT v.id, v.date, a.kind, a.reversal_voucher_id, a.opening_original_date IS NOT NULL FROM party_advances a
             JOIN vouchers v ON v.id = a.voucher_id AND v.company_id = a.company_id
             WHERE a.company_id = ?1 AND a.id = ?2",
                params![company, advance],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
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
        if voucher_has_bill_settlements(&tx, company, original)? {
            let voucher = crate::inventory::reverse_settlement_on_connection(
                &tx, company, original, date, reference, reason,
            )?;
            tx.commit()?;
            return Ok(voucher);
        }
        check_advance_reversal(&tx, company, advance, date)?;
        let kind = if opening {
            VoucherKind::Journal
        } else {
            match InvoiceKind::parse(&kind)? {
                InvoiceKind::Sale => VoucherKind::Payment,
                InvoiceKind::Purchase => VoucherKind::Receipt,
            }
        };
        let date = date.format("%Y-%m-%d").to_string();
        let postings = if opening {
            crate::reversals::checked_counter_postings(&tx, company, original, &date)?
        } else {
            counter_postings(&tx, company, original, &date)?
        };
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
        read_party_advances(&self.conn, company, through.map(parse_date).transpose()?)
    }
}

fn voucher_has_bill_settlements(conn: &Connection, company: i64, voucher: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM invoice_settlements WHERE company_id = ?1 AND voucher_id = ?2)",
        params![company, voucher],
        |row| row.get(0),
    )?)
}

pub(crate) fn check_advance_reversal(
    conn: &Connection,
    company: i64,
    advance: i64,
    date: NaiveDate,
) -> Result<()> {
    let state = advance_allocation_state(conn, company, advance, None)?;
    if state.applied > 0 {
        return Err(Error::Validation(
            "Release active allocations before reversing this advance".into(),
        ));
    }
    if state.latest_release.is_some_and(|released| date < released) {
        return Err(Error::Validation(
            "Reversal date cannot precede an allocation release".into(),
        ));
    }
    Ok(())
}

pub(crate) fn read_party_advances(
    conn: &Connection,
    company: i64,
    through: Option<NaiveDate>,
) -> Result<Vec<PartyAdvance>> {
    let mut stmt = conn.prepare(
            "SELECT a.id, a.kind, a.party_ledger_id, party.name, bank.name, v.id, v.date, v.reference,
                    v.narration, a.amount, rv.id, rv.date, rv.reference, a.reversal_reason,
                    a.source_bill_type, a.source_bill_reference, a.source_bill_due_date, a.source_bill_credit_days, a.opening_original_date,
                    EXISTS(SELECT 1 FROM invoice_settlements s WHERE s.voucher_id = a.voucher_id AND s.company_id = a.company_id)
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
        let id = row.get(0)?;
        let amount = row.get::<_, i64>(9)?;
        let applied = advance_allocation_state(conn, company, id, through)?.applied;
        let available = if reversal.is_some() {
            0
        } else {
            i128::from(amount) - applied
        };
        if available < 0 {
            return Err(Error::Validation(
                "Allocations exceed the advance amount".into(),
            ));
        }
        let source_bill = match row.get::<_, Option<String>>(14)? {
            Some(kind) => Some(AdvanceSourceBill {
                kind,
                reference: row.get(15)?,
                due_date: row.get(16)?,
                credit_days: row.get(17)?,
            }),
            None => None,
        };
        advances.push((
            original_date,
            PartyAdvance {
                id,
                kind: InvoiceKind::parse(&row.get::<_, String>(1)?)?,
                party_ledger_id: row.get(2)?,
                party: row.get(3)?,
                bank: row.get(4)?,
                voucher_id: row.get(5)?,
                date,
                reference: row.get(7)?,
                narration: row.get(8)?,
                amount,
                opening_original_date: row.get(18)?,
                includes_settlements: row.get(19)?,
                applied,
                available,
                source_bill,
                reversal,
            },
        ));
    }
    advances.sort_by(|(a, row_a), (b, row_b)| b.cmp(a).then_with(|| row_b.id.cmp(&row_a.id)));
    Ok(advances.into_iter().map(|(_, advance)| advance).collect())
}

pub(crate) fn migrate_allocations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE advance_allocations(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            advance_id INTEGER NOT NULL REFERENCES party_advances(id),
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            date TEXT NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0),
            narration TEXT NOT NULL,
            released_on TEXT,
            release_reason TEXT,
            CHECK((released_on IS NULL) = (release_reason IS NULL))
        );
        CREATE INDEX advance_allocations_advance ON advance_allocations(company_id, advance_id, id);
        CREATE INDEX advance_allocations_invoice ON advance_allocations(company_id, invoice_id, id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct AdvanceAllocation {
    pub id: i64,
    pub invoice_id: i64,
    pub invoice_reference: String,
    pub bill_reference: String,
    pub date: String,
    pub amount: i64,
    pub narration: String,
    pub released_on: Option<String>,
    pub release_reason: Option<String>,
}

#[derive(Default)]
pub(crate) struct AllocationState {
    pub applied: i128,
    pub has_history: bool,
    pub latest_release: Option<NaiveDate>,
    pub latest_active_date: Option<NaiveDate>,
}

enum AllocationTarget {
    Advance(i64),
    Invoice(i64),
}
fn allocation_state(
    conn: &Connection,
    company: i64,
    target: AllocationTarget,
    through: Option<NaiveDate>,
) -> Result<AllocationState> {
    let (column, id) = match target {
        AllocationTarget::Advance(id) => ("advance_id", id),
        AllocationTarget::Invoice(id) => ("invoice_id", id),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT date, amount, released_on FROM advance_allocations WHERE company_id = ?1 AND {column} = ?2"
    ))?;
    let rows = stmt.query(params![company, id])?;
    read_allocation_state(rows, through)
}

pub(crate) fn read_allocation_state(
    mut rows: rusqlite::Rows<'_>,
    through: Option<NaiveDate>,
) -> Result<AllocationState> {
    let mut state = AllocationState::default();
    while let Some(row) = rows.next()? {
        state.has_history = true;
        let date = parse_date(&row.get::<_, String>(0)?)?;
        let amount = row.get::<_, i64>(1)?;
        let released = row
            .get::<_, Option<String>>(2)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        if let Some(date) = released {
            state.latest_release = state.latest_release.max(Some(date));
        }
        let active = match through {
            Some(end) => date <= end && released.is_none_or(|released| end < released),
            None => released.is_none(),
        };
        if active {
            state.applied += i128::from(amount);
            state.latest_active_date = state.latest_active_date.max(Some(date));
        }
    }
    Ok(state)
}

pub(crate) fn advance_allocation_state(
    conn: &Connection,
    company: i64,
    advance: i64,
    through: Option<NaiveDate>,
) -> Result<AllocationState> {
    allocation_state(conn, company, AllocationTarget::Advance(advance), through)
}

pub(crate) fn invoice_allocation_state(
    conn: &Connection,
    company: i64,
    invoice: i64,
    through: Option<NaiveDate>,
) -> Result<AllocationState> {
    allocation_state(conn, company, AllocationTarget::Invoice(invoice), through)
}

pub(crate) fn check_open_date(conn: &Connection, company: i64, date: NaiveDate) -> Result<()> {
    if crate::periods::cutoff(conn, company)?.is_some_and(|cutoff| date <= cutoff) {
        return Err(Error::Validation(
            crate::periods::LOCKED_PERIOD_MESSAGE.into(),
        ));
    }
    Ok(())
}

pub(crate) fn insert_advance_allocation(
    conn: &Connection,
    company: i64,
    advance: i64,
    target: BillSettlementTarget,
    date: NaiveDate,
    amount: i64,
    narration: &str,
) -> Result<i64> {
    let invoice = target.invoice_id;
    if amount <= 0 {
        return Err(Error::Validation(
            "Enter a positive allocation amount".into(),
        ));
    }
    check_open_date(conn, company, date)?;
    let (kind, party, advance_date, advance_amount, reversed): (String, i64, String, i64, bool) = conn.query_row(
            "SELECT a.kind, a.party_ledger_id, v.date, a.amount, a.reversal_voucher_id IS NOT NULL
             FROM party_advances a JOIN vouchers v ON v.id = a.voucher_id AND v.company_id = a.company_id
             WHERE a.company_id = ?1 AND a.id = ?2",
            params![company, advance], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Advance not found in this company".into()))?;
    if reversed {
        return Err(Error::Validation(
            "A reversed advance cannot be applied".into(),
        ));
    }
    let (invoice_kind, invoice_party, invoice_date, tax): (String, i64, String, i64) = conn
        .query_row(
            "SELECT i.kind, i.party_ledger_id, v.date, i.tax_amount FROM invoices i
             JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
             WHERE i.company_id = ?1 AND i.id = ?2",
            params![company, invoice],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| Error::Validation("Invoice not found in this company".into()))?;
    if party != invoice_party || kind != invoice_kind {
        return Err(Error::Validation(
            "Advance and invoice must have the same party and type".into(),
        ));
    }
    if date < parse_date(&invoice_date)? || date < parse_date(&advance_date)? {
        return Err(Error::Validation(
            "Allocation date cannot precede the advance or invoice date".into(),
        ));
    }
    if crate::invoice_reversals::for_invoice(conn, company, invoice)?.is_some() {
        return Err(Error::Validation(
            "A reversed invoice cannot accept an advance".into(),
        ));
    }
    let bill = crate::bill_splits::target(conn, target)?;
    let advance_state = advance_allocation_state(conn, company, advance, None)?;
    let invoice_state = invoice_allocation_state(conn, company, invoice, None)?;
    let credits = crate::invoice_credits::state(conn, company, invoice, None)?;
    if advance_state
        .latest_release
        .max(invoice_state.latest_release)
        .max(credits.latest_reversal)
        .is_some_and(|released| date < released)
    {
        return Err(Error::Validation(
            "Allocation date cannot precede an advance release or note reversal".into(),
        ));
    }
    if i128::from(amount) > i128::from(advance_amount) - advance_state.applied {
        return Err(Error::Validation(
            "Allocation exceeds the available advance amount".into(),
        ));
    }
    let mut paid = 0_i128;
    {
        let mut stmt = conn.prepare(
                "SELECT s.amount, rv.date FROM invoice_settlements s
                 LEFT JOIN settlement_reversals r ON r.settlement_voucher_id = s.voucher_id AND r.company_id = s.company_id
                 LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
                 WHERE s.company_id = ?1 AND s.invoice_id = ?2",
            )?;
        let mut rows = stmt.query(params![company, invoice])?;
        while let Some(row) = rows.next()? {
            if let Some(reversed_on) = row.get::<_, Option<String>>(1)? {
                if date < parse_date(&reversed_on)? {
                    return Err(Error::Validation(
                        "Allocation date cannot precede a settlement reversal".into(),
                    ));
                }
            } else {
                paid += i128::from(row.get::<_, i64>(0)?);
            }
        }
    }
    if i128::from(amount)
        > invoice_total(conn, invoice, tax)? - paid - invoice_state.applied - credits.applied
    {
        return Err(Error::Validation(
            "Allocation exceeds the outstanding invoice amount".into(),
        ));
    }
    if let Some(bill) = &bill {
        if i128::from(amount) > bill.outstanding() {
            return Err(Error::Validation(format!(
                "Allocation exceeds the outstanding amount for bill {}",
                bill.reference
            )));
        }
    }
    conn.execute(
            "INSERT INTO advance_allocations(company_id, advance_id, invoice_id, date, amount, narration)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![company, advance, invoice, date.format("%Y-%m-%d").to_string(), amount, narration.trim()],
        )?;
    let id = conn.last_insert_rowid();
    if let Some(bill) = bill {
        conn.execute(
            "INSERT INTO bill_split_advances(allocation_id, split_id, invoice_id)
                 VALUES (?1, ?2, ?3)",
            params![id, bill.id, invoice],
        )?;
    }
    Ok(id)
}

impl Store {
    /// Apply recorded cash on account to an invoice without posting a second receipt or payment.
    pub fn apply_party_advance(
        &mut self,
        company: i64,
        advance: i64,
        invoice: i64,
        date: &str,
        amount: i64,
        narration: &str,
    ) -> Result<i64> {
        self.apply_party_advance_to_bill(
            company,
            advance,
            BillSettlementTarget {
                invoice_id: invoice,
                split_id: None,
            },
            date,
            amount,
            narration,
        )
    }

    /// Apply an advance to a named bill without posting another cash movement.
    pub fn apply_party_advance_to_bill(
        &mut self,
        company: i64,
        advance: i64,
        target: BillSettlementTarget,
        date: &str,
        amount: i64,
        narration: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = insert_advance_allocation(&tx, company, advance, target, date, amount, narration)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn release_advance_allocation(
        &mut self,
        company: i64,
        allocation: i64,
        date: &str,
        reason: &str,
    ) -> Result<()> {
        let date = parse_date(date)?;
        let reason = required(reason, "Reason for releasing the allocation")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_open_date(&tx, company, date)?;
        let (original_date, released): (String, bool) = tx.query_row(
            "SELECT date, released_on IS NOT NULL FROM advance_allocations WHERE company_id = ?1 AND id = ?2",
            params![company, allocation], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Allocation not found in this company".into()))?;
        if released {
            return Err(Error::Validation(
                "This allocation was already released".into(),
            ));
        }
        if date < parse_date(&original_date)? {
            return Err(Error::Validation(
                "Release cannot precede the allocation date".into(),
            ));
        }
        tx.execute(
            "UPDATE advance_allocations SET released_on = ?1, release_reason = ?2 WHERE company_id = ?3 AND id = ?4",
            params![date.format("%Y-%m-%d").to_string(), reason, company, allocation],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn advance_allocations(
        &self,
        company: i64,
        advance: i64,
        through: Option<&str>,
    ) -> Result<Vec<AdvanceAllocation>> {
        let through = through.map(parse_date).transpose()?;
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.invoice_id, v.reference, a.date, a.amount, a.narration, a.released_on,
                    a.release_reason, COALESCE(b.reference, i.bill_reference, v.reference)
             FROM advance_allocations a JOIN invoices i ON i.id = a.invoice_id AND i.company_id = a.company_id
             JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
             LEFT JOIN bill_split_advances s ON s.allocation_id = a.id AND s.invoice_id = a.invoice_id
             LEFT JOIN invoice_bill_splits b ON b.id = s.split_id AND b.invoice_id = i.id
             WHERE a.company_id = ?1 AND a.advance_id = ?2 ORDER BY a.date, a.id",
        )?;
        let mut rows = stmt.query(params![company, advance])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let date: String = row.get(3)?;
            let parsed = parse_date(&date)?;
            if through.is_some_and(|end| parsed > end) {
                continue;
            }
            let mut released_on: Option<String> = row.get(6)?;
            let mut release_reason: Option<String> = row.get(7)?;
            let released_date = released_on.as_deref().map(parse_date).transpose()?;
            if matches!((released_date, through), (Some(released), Some(end)) if released > end) {
                released_on = None;
                release_reason = None;
            }
            result.push(AdvanceAllocation {
                id: row.get(0)?,
                invoice_id: row.get(1)?,
                invoice_reference: row.get(2)?,
                bill_reference: row.get(8)?,
                date,
                amount: row.get(4)?,
                narration: row.get(5)?,
                released_on,
                release_reason,
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bill_splits::BillSplitDraft;
    use crate::inventory::{
        BillSettlementAllocation, InvoiceAdjustment, InvoiceDraft, InvoiceEntryMode,
    };

    #[test]
    fn advance_reduces_only_its_split_bill_until_released() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let bank = store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        let income = store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let advance = store
            .post_party_advance(
                company,
                &AdvanceDraft {
                    kind: InvoiceKind::Sale,
                    party_ledger_id: customer,
                    bank_ledger_id: bank,
                    date: "2026-01-01".into(),
                    reference: "A-1".into(),
                    amount: 6000,
                    narration: String::new(),
                },
            )
            .unwrap();
        let invoice = store
            .create_invoice(
                company,
                &InvoiceDraft {
                    kind: InvoiceKind::Sale,
                    entry_mode: InvoiceEntryMode::Invoice,
                    date: "2026-01-02".into(),
                    reference: "I-1".into(),
                    narration: String::new(),
                    due_date: None,
                    credit_days: None,
                    bill_reference: None,
                    bill_splits: vec![
                        BillSplitDraft {
                            reference: "FIRST".into(),
                            amount: 4000,
                            due_date: None,
                            credit_days: Some(10),
                        },
                        BillSplitDraft {
                            reference: "SECOND".into(),
                            amount: 6000,
                            due_date: None,
                            credit_days: Some(20),
                        },
                    ],
                    party_ledger_id: customer,
                    trade_ledger_id: income,
                    tax_ledger_id: None,
                    tax_amount: 0,
                    additional_taxes: Vec::new(),
                    tax_calculation: None,
                    lines: Vec::new(),
                    adjustments: vec![InvoiceAdjustment {
                        ledger_id: income,
                        amount: 10000,
                        description: "Service".into(),
                    }],
                },
            )
            .unwrap();
        let bills = store.invoices(company).unwrap().remove(0).bill_splits;
        let target = |index: usize| BillSettlementTarget {
            invoice_id: invoice,
            split_id: Some(bills[index].id),
        };
        assert!(store
            .apply_party_advance(company, advance, invoice, "2026-01-04", 3000, "")
            .is_err());
        assert!(store
            .apply_party_advance_to_bill(company, advance, target(0), "2026-01-04", 4500, "")
            .is_err());
        let allocation = store
            .apply_party_advance_to_bill(company, advance, target(0), "2026-01-04", 3000, "")
            .unwrap();
        assert!(store
            .settle_bills(
                company,
                bank,
                "2026-01-05",
                "OVER",
                &[BillSettlementAllocation {
                    target: target(0),
                    amount: 1001
                },]
            )
            .is_err());
        store
            .settle_bills(
                company,
                bank,
                "2026-01-05",
                "R-1",
                &[BillSettlementAllocation {
                    target: target(1),
                    amount: 2000,
                }],
            )
            .unwrap();
        store
            .release_advance_allocation(company, allocation, "2026-01-07", "Apply elsewhere")
            .unwrap();
        let balances = |date| {
            store
                .outstanding_invoices(company, date)
                .unwrap()
                .into_iter()
                .map(|row| (row.outstanding, row.advance_applied))
                .collect::<Vec<_>>()
        };
        assert_eq!(balances("2026-01-03"), [(4000, 0), (6000, 0)]);
        assert_eq!(balances("2026-01-06"), [(1000, 3000), (4000, 0)]);
        assert_eq!(balances("2026-01-07"), [(4000, 0), (4000, 0)]);
        let history = store.advance_allocations(company, advance, None).unwrap();
        assert_eq!(history[0].bill_reference, "FIRST");
        assert_eq!(history[0].released_on.as_deref(), Some("2026-01-07"));
        assert_eq!(store.vouchers(company).unwrap().len(), 3);
    }
}
