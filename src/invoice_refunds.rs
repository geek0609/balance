//! Refundable credit and cash movements against posted invoice notes.

use crate::inventory::{ledger_category, InvoiceKind};
use crate::{
    insert_voucher_draft, parse_date, required, Category, Error, Posting, Result, Store,
    VoucherDraft, VoucherKind,
};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE invoice_refund_credits(
            note_id INTEGER PRIMARY KEY REFERENCES invoice_credit_notes(id),
            amount INTEGER NOT NULL CHECK(amount > 0)
        );
        CREATE TABLE invoice_credit_refunds(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            note_id INTEGER NOT NULL REFERENCES invoice_refund_credits(note_id),
            voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id),
            bank_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            reference_key TEXT NOT NULL,
            amount INTEGER NOT NULL CHECK(amount > 0),
            reversal_voucher_id INTEGER UNIQUE REFERENCES vouchers(id),
            reversal_reason TEXT,
            CHECK((reversal_voucher_id IS NULL) = (reversal_reason IS NULL)),
            UNIQUE(note_id, reference_key)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct InvoiceRefund {
    pub id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub bank: String,
    pub amount: i64,
    pub reversal_date: Option<String>,
    pub reversal_reference: Option<String>,
    pub reversal_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RefundBalance {
    pub note_id: i64,
    pub invoice_id: i64,
    pub invoice_reference: String,
    pub note_reference: String,
    pub note_date: String,
    pub party: String,
    pub kind: InvoiceKind,
    pub credit: i64,
    pub outstanding: i64,
}

#[derive(Default)]
struct RefundState {
    spent: i128,
    latest_reversal: Option<NaiveDate>,
}

fn state(
    conn: &Connection,
    company: i64,
    note: i64,
    through: Option<NaiveDate>,
) -> Result<RefundState> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.amount, v.date, rv.date FROM invoice_credit_refunds r
         JOIN vouchers v ON v.id = r.voucher_id AND v.company_id = r.company_id
         LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id AND rv.company_id = r.company_id
         WHERE r.company_id = ?1 AND r.note_id = ?2
         UNION ALL SELECT a.amount, a.date, a.released_on FROM invoice_credit_allocations a
         WHERE a.company_id = ?1 AND a.note_id = ?2",
    )?;
    let mut rows = stmt.query(params![company, note])?;
    let mut state = RefundState::default();
    while let Some(row) = rows.next()? {
        let date = parse_date(&row.get::<_, String>(1)?)?;
        let reversed = row
            .get::<_, Option<String>>(2)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        state.latest_reversal = state.latest_reversal.max(reversed);
        if through.is_none_or(|end| date <= end)
            && reversed.is_none_or(|day| through.is_some_and(|end| end < day))
        {
            state.spent += i128::from(row.get::<_, i64>(0)?);
        }
    }
    Ok(state)
}

pub(crate) fn check_note_reversal(
    conn: &Connection,
    company: i64,
    note: i64,
    date: NaiveDate,
) -> Result<()> {
    let refunds = state(conn, company, note, None)?;
    if refunds.spent > 0 {
        return Err(Error::Validation(
            "Reverse refunds and release applied credit before reversing this note".into(),
        ));
    }
    if refunds.latest_reversal.is_some_and(|day| date < day) {
        return Err(Error::Validation(
            "The note reversal cannot precede a refund reversal or credit release".into(),
        ));
    }
    Ok(())
}

pub(crate) fn available_credit(
    conn: &Connection,
    company: i64,
    note: i64,
    date: NaiveDate,
) -> Result<(InvoiceKind, i64, i128)> {
    let (credit, note_date, kind, party): (i64, String, String, i64) = conn
        .query_row(
            "SELECT c.amount, v.date, i.kind, i.party_ledger_id FROM invoice_refund_credits c
         JOIN invoice_credit_notes n ON n.id = c.note_id AND n.company_id = ?1
         JOIN invoices i ON i.id = n.invoice_id AND i.company_id = n.company_id
         JOIN vouchers v ON v.id = n.voucher_id AND v.company_id = n.company_id
         WHERE n.id = ?2 AND n.reversal_voucher_id IS NULL",
            params![company, note],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| Error::Validation("Select an active note with refundable credit".into()))?;
    let refund = state(conn, company, note, None)?;
    if date < parse_date(&note_date)? || refund.latest_reversal.is_some_and(|day| date < day) {
        return Err(Error::Validation(
            "Date cannot precede the note, refund reversal or credit release".into(),
        ));
    }
    Ok((
        InvoiceKind::parse(&kind)?,
        party,
        i128::from(credit) - refund.spent,
    ))
}

impl Store {
    pub fn invoice_refunds(&self, company: i64, note: i64) -> Result<Vec<InvoiceRefund>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT r.id, v.id, v.date, v.reference, bank.name, r.amount, rv.date, rv.reference, r.reversal_reason
             FROM invoice_credit_refunds r JOIN vouchers v ON v.id = r.voucher_id AND v.company_id = r.company_id
             JOIN ledgers bank ON bank.id = r.bank_ledger_id AND bank.company_id = r.company_id
             LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id AND rv.company_id = r.company_id
             WHERE r.company_id = ?1 AND r.note_id = ?2 ORDER BY v.date, v.id",
        )?;
        let rows = stmt
            .query_map(params![company, note], |row| {
                Ok(InvoiceRefund {
                    id: row.get(0)?,
                    voucher_id: row.get(1)?,
                    date: row.get(2)?,
                    reference: row.get(3)?,
                    bank: row.get(4)?,
                    amount: row.get(5)?,
                    reversal_date: row.get(6)?,
                    reversal_reference: row.get(7)?,
                    reversal_reason: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn invoice_refund_balances(
        &self,
        company: i64,
        through: &str,
    ) -> Result<Vec<RefundBalance>> {
        let through = parse_date(through)?;
        let snapshot = self.conn.unchecked_transaction()?;
        let balances = {
            let mut stmt = snapshot.prepare(
                "SELECT n.id, i.id, iv.reference, v.reference, v.date, party.name, i.kind, c.amount, rv.date
                 FROM invoice_refund_credits c JOIN invoice_credit_notes n ON n.id = c.note_id
                 JOIN invoices i ON i.id = n.invoice_id AND i.company_id = n.company_id
                 JOIN ledgers party ON party.id = i.party_ledger_id AND party.company_id = i.company_id
                 JOIN vouchers iv ON iv.id = i.voucher_id
                 JOIN vouchers v ON v.id = n.voucher_id
                 LEFT JOIN vouchers rv ON rv.id = n.reversal_voucher_id
                 WHERE n.company_id = ?1 ORDER BY party.name, v.date, v.id",
            )?;
            let mut rows = stmt.query([company])?;
            let mut balances = Vec::new();
            while let Some(row) = rows.next()? {
                let date: String = row.get(4)?;
                let reversed = row
                    .get::<_, Option<String>>(8)?
                    .as_deref()
                    .map(parse_date)
                    .transpose()?;
                if parse_date(&date)? > through || reversed.is_some_and(|day| day <= through) {
                    continue;
                }
                let note = row.get(0)?;
                let credit: i64 = row.get(7)?;
                let outstanding = i64::try_from(
                    i128::from(credit) - state(&snapshot, company, note, Some(through))?.spent,
                )
                .map_err(|_| Error::Validation("Refund balance is out of range".into()))?;
                if outstanding < 0 {
                    return Err(Error::Validation(
                        "Refunds and allocations exceed the credit on an invoice note".into(),
                    ));
                }
                if outstanding == 0 {
                    continue;
                }
                balances.push(RefundBalance {
                    note_id: note,
                    invoice_id: row.get(1)?,
                    invoice_reference: row.get(2)?,
                    note_reference: row.get(3)?,
                    note_date: date,
                    party: row.get(5)?,
                    kind: InvoiceKind::parse(&row.get::<_, String>(6)?)?,
                    credit,
                    outstanding,
                });
            }
            balances
        };
        snapshot.commit()?;
        Ok(balances)
    }

    pub fn post_invoice_refund(
        &mut self,
        company: i64,
        note: i64,
        bank: i64,
        date: &str,
        reference: &str,
        amount: i64,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Refund reference")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (kind, party, available) = available_credit(&tx, company, note, date)?;
        if amount <= 0 || i128::from(amount) > available {
            return Err(Error::Validation(
                "Enter a positive refund within the remaining credit".into(),
            ));
        }
        if bank == party || ledger_category(&tx, company, bank)? != Category::Asset {
            return Err(Error::Validation(
                "Select a cash or bank asset ledger different from the party".into(),
            ));
        }
        let (kind, cash_amount) = match kind {
            InvoiceKind::Sale => (VoucherKind::Payment, -amount),
            InvoiceKind::Purchase => (VoucherKind::Receipt, amount),
        };
        let postings = [
            Posting {
                ledger_id: bank,
                amount: cash_amount,
            },
            Posting {
                ledger_id: party,
                amount: -cash_amount,
            },
        ];
        let voucher = insert_voucher_draft(
            &tx,
            company,
            VoucherDraft {
                kind,
                reference,
                date: &date.to_string(),
                narration: &format!("Refund against invoice note {note}"),
                postings: &postings,
                party_lines: &[],
            },
        )?;
        if tx.execute(
            "INSERT INTO invoice_credit_refunds(company_id, note_id, voucher_id, bank_ledger_id, reference_key, amount)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(note_id, reference_key) DO NOTHING",
            params![company, note, voucher, bank, reference.to_lowercase(), amount],
        )? != 1 {
            return Err(Error::Validation("This note already has a refund with that reference".into()));
        }
        tx.commit()?;
        Ok(voucher)
    }

    pub fn reverse_invoice_refund(
        &mut self,
        company: i64,
        refund: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Refund reversal reference")?;
        let reason = required(reason, "Refund reversal reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (original, posted): (i64, String) = tx.query_row(
            "SELECT v.id, v.date FROM invoice_credit_refunds r JOIN vouchers v ON v.id = r.voucher_id AND v.company_id = r.company_id
             WHERE r.company_id = ?1 AND r.id = ?2 AND r.reversal_voucher_id IS NULL",
            params![company, refund], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Select an active refund".into()))?;
        if date < parse_date(&posted)? {
            return Err(Error::Validation(
                "Refund reversal date cannot precede the original refund".into(),
            ));
        }
        let date = date.to_string();
        let postings = crate::reversals::counter_postings(&tx, company, original, &date)?;
        tx.execute(
            "INSERT INTO vouchers(company_id, date, narration, kind, reference) VALUES (?1, ?2, ?3, 'Journal', ?4)",
            params![company, date, format!("Refund {refund} reversal: {reason}"), reference],
        )?;
        let voucher = tx.last_insert_rowid();
        crate::reversals::write_counter_postings(&tx, company, original, voucher, &postings)?;
        tx.execute("UPDATE invoice_credit_refunds SET reversal_voucher_id = ?1, reversal_reason = ?2 WHERE company_id = ?3 AND id = ?4",
            params![voucher, reason, company, refund])?;
        tx.commit()?;
        Ok(voucher)
    }
}

#[cfg(test)]
mod tests {
    use crate::inventory::{
        InvoiceAdjustment, InvoiceDiscount, InvoiceDraft, InvoiceKind, InvoiceLine,
    };
    use crate::invoice_credits::CreditLineDraft;
    use crate::invoice_returns::ReturnLineDraft;
    use crate::{Category, Store, VoucherKind};

    fn invoice(kind: InvoiceKind, party: i64, trade: i64) -> InvoiceDraft {
        InvoiceDraft {
            external_reference: Default::default(),
            kind,
            entry_mode: crate::inventory::InvoiceEntryMode::Invoice,
            date: "2026-03-01".into(),
            due_date: None,
            credit_days: None,
            reference: "INV-01".into(),
            bill_reference: None,
            bill_splits: Vec::new(),
            narration: String::new(),
            party_ledger_id: party,
            trade_ledger_id: trade,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: vec![],
            tax_calculation: None,
            adjustments: vec![],
            lines: vec![],
        }
    }

    #[test]
    fn partial_payment_splits_credit_and_refunds_by_date() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Demo").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let bank = store
            .create_ledger(company, "Cash", Category::Asset)
            .unwrap();
        let sales = store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let mut draft = invoice(InvoiceKind::Sale, customer, sales);
        draft.adjustments.push(InvoiceAdjustment {
            ledger_id: sales,
            amount: 1000,
            description: "Service".into(),
        });
        let invoice = store.create_invoice(company, &draft).unwrap();
        store
            .settle_invoice(company, invoice, bank, "2026-03-02", "PAY-01", 600)
            .unwrap();
        let lines = [CreditLineDraft {
            ledger_id: sales,
            is_tax: false,
            amount: 800,
        }];
        assert!(store
            .post_invoice_credit(
                company,
                invoice,
                "2026-03-01",
                "CR-01",
                "Price correction",
                &lines
            )
            .is_err());
        let note = store
            .post_invoice_credit(
                company,
                invoice,
                "2026-03-03",
                "CR-01",
                "Price correction",
                &lines,
            )
            .unwrap();
        let current = &store.invoices(company).unwrap()[0];
        assert_eq!(
            (current.credited, current.credit_total, current.outstanding),
            (400, 800, 0)
        );
        assert_eq!(
            store.outstanding_invoices(company, "2026-03-02").unwrap()[0].outstanding,
            400
        );
        assert!(store
            .outstanding_invoices(company, "2026-03-03")
            .unwrap()
            .is_empty());
        assert!(store
            .invoice_refund_balances(company, "2026-03-02")
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .invoice_refund_balances(company, "2026-03-03")
                .unwrap()[0]
                .outstanding,
            400
        );
        assert!(store
            .post_invoice_refund(company, note, bank, "2026-03-04", "RF-01", 401)
            .is_err());
        let voucher = store
            .post_invoice_refund(company, note, bank, "2026-03-04", "RF-01", 250)
            .unwrap();
        let saved = store
            .vouchers(company)
            .unwrap()
            .into_iter()
            .find(|row| row.id == voucher)
            .unwrap();
        assert_eq!(saved.kind, VoucherKind::Payment);
        assert!(saved
            .postings
            .iter()
            .any(|line| line.ledger_id == bank && line.amount == -250));
        assert!(store
            .reverse_voucher(company, voucher, "2026-03-04", "VOID-01", "Wrong route")
            .is_err());
        assert!(store
            .reverse_invoice_credit(company, note, "2026-03-05", "VOID-02", "Undo note")
            .is_err());
        let refund = store.invoice_refunds(company, note).unwrap()[0].id;
        store
            .reverse_invoice_refund(company, refund, "2026-03-06", "RF-REV", "Returned funds")
            .unwrap();
        assert_eq!(
            store
                .invoice_refund_balances(company, "2026-03-05")
                .unwrap()[0]
                .outstanding,
            150
        );
        assert_eq!(
            store
                .invoice_refund_balances(company, "2026-03-06")
                .unwrap()[0]
                .outstanding,
            400
        );
        assert!(store
            .post_invoice_refund(company, note, bank, "2026-03-05", "RF-02", 400)
            .is_err());
        assert!(store
            .reverse_invoice_credit(company, note, "2026-03-05", "VOID-02", "Too early")
            .is_err());
        store
            .reverse_invoice_credit(company, note, "2026-03-07", "VOID-02", "Undo note")
            .unwrap();
        assert!(store
            .invoice_refund_balances(company, "2026-03-07")
            .unwrap()
            .is_empty());
        assert_eq!(store.invoices(company).unwrap()[0].outstanding, 400);
    }

    #[test]
    fn paid_purchase_return_creates_credit_and_cash_receipt() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Demo").unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let bank = store
            .create_ledger(company, "Cash", Category::Asset)
            .unwrap();
        let purchases = store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let mut draft = invoice(InvoiceKind::Purchase, supplier, purchases);
        draft.lines.push(InvoiceLine {
            billed_quantity: None,
            rate_basis: None,
            item_id: item,
            location_id: location,
            batch_id: None,
            trade_ledger_id: None,
            extra_allocations: vec![],
            quantity: 1000,
            unit_price: 1000,
            discount: InvoiceDiscount::None,
        });
        let invoice = store.create_invoice(company, &draft).unwrap();
        store
            .settle_invoice(company, invoice, bank, "2026-03-02", "PAY-01", 1000)
            .unwrap();
        let allocation = store.invoice_return_allocations(company, invoice).unwrap()[0].id;
        let note = store
            .post_invoice_stock_return(
                company,
                invoice,
                "2026-03-03",
                "RET-01",
                "Returned part",
                &[ReturnLineDraft {
                    allocation,
                    quantity: 1000,
                }],
                &[],
            )
            .unwrap();
        let info = &store.invoice_credit_notes(company, invoice).unwrap()[0];
        assert_eq!(info.refundable, 1000);
        assert_eq!(store.invoices(company).unwrap()[0].credited, 0);
        assert_eq!(store.invoices(company).unwrap()[0].outstanding, 0);
        assert_eq!(store.stock_balances(company, None).unwrap()[0].quantity, 0);
        let voucher = store
            .post_invoice_refund(company, note, bank, "2026-03-04", "RF-01", 1000)
            .unwrap();
        let saved = store
            .vouchers(company)
            .unwrap()
            .into_iter()
            .find(|row| row.id == voucher)
            .unwrap();
        assert_eq!(saved.kind, VoucherKind::Receipt);
        assert!(saved
            .postings
            .iter()
            .any(|line| line.ledger_id == bank && line.amount == 1000));
        assert!(store
            .invoice_refund_balances(company, "2026-03-04")
            .unwrap()
            .is_empty());
        assert!(store
            .reverse_invoice(company, invoice, "2026-03-04", "VOID-01", "Active return")
            .is_err());
    }

    #[test]
    fn credit_application_reduces_a_bill_without_a_cash_entry_and_releases_by_date() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Demo").unwrap();
        let party = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let other = store
            .create_ledger(company, "Other customer", Category::Asset)
            .unwrap();
        let bank = store
            .create_ledger(company, "Cash", Category::Asset)
            .unwrap();
        let sales = store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let mut draft = invoice(InvoiceKind::Sale, party, sales);
        draft.adjustments.push(InvoiceAdjustment {
            ledger_id: sales,
            amount: 1000,
            description: "Service".into(),
        });
        let original = store.create_invoice(company, &draft).unwrap();
        store
            .settle_invoice(company, original, bank, "2026-03-02", "PAY-01", 700)
            .unwrap();
        let note = store
            .post_invoice_credit(
                company,
                original,
                "2026-03-03",
                "CR-01",
                "Allowance",
                &[CreditLineDraft {
                    ledger_id: sales,
                    amount: 400,
                    is_tax: false,
                }],
            )
            .unwrap();
        draft.reference = "INV-02".into();
        draft.adjustments[0].amount = 200;
        let target = store.create_invoice(company, &draft).unwrap();
        draft.party_ledger_id = other;
        let wrong_party = store.create_invoice(company, &draft).unwrap();
        let vouchers = store.vouchers(company).unwrap().len();
        assert!(store
            .apply_invoice_credit(company, note, wrong_party, "2026-03-04", 60)
            .is_err());
        assert!(store
            .apply_invoice_credit(company, note, target, "2026-03-02", 60)
            .is_err());
        let allocation = store
            .apply_invoice_credit(company, note, target, "2026-03-04", 60)
            .unwrap();
        assert_eq!(store.vouchers(company).unwrap().len(), vouchers);
        let due = |store: &Store, date: &str| {
            store
                .outstanding_invoices(company, date)
                .unwrap()
                .iter()
                .find(|row| row.invoice_id == target)
                .unwrap()
                .outstanding
        };
        assert_eq!(due(&store, "2026-03-03"), 200);
        assert_eq!(due(&store, "2026-03-04"), 140);
        assert_eq!(
            store
                .invoice_refund_balances(company, "2026-03-04")
                .unwrap()[0]
                .outstanding,
            40
        );
        assert!(store
            .settle_invoice(company, target, bank, "2026-03-04", "PAY-02", 141)
            .is_err());
        assert!(store
            .post_invoice_refund(company, note, bank, "2026-03-04", "REF-01", 41)
            .is_err());
        assert!(store
            .reverse_invoice_credit(company, note, "2026-03-04", "REV-01", "Undo")
            .is_err());
        assert!(store
            .reverse_invoice(company, target, "2026-03-04", "REV-02", "Undo")
            .is_err());
        store
            .set_period_lock(company, Some("2026-03-04"), "Close books")
            .unwrap();
        assert!(store
            .release_invoice_credit(company, allocation, "2026-03-04", "Moved")
            .is_err());
        store
            .release_invoice_credit(company, allocation, "2026-03-05", "Moved")
            .unwrap();
        assert_eq!(due(&store, "2026-03-04"), 140);
        assert_eq!(due(&store, "2026-03-05"), 200);
        assert_eq!(
            store
                .invoice_refund_balances(company, "2026-03-05")
                .unwrap()[0]
                .outstanding,
            100
        );
        store
            .post_invoice_refund(company, note, bank, "2026-03-05", "REF-01", 100)
            .unwrap();
        assert!(store
            .invoice_refund_balances(company, "2026-03-05")
            .unwrap()
            .is_empty());
    }
}
