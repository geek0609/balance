//! Dated inventory journals and their warehouse movements.
use crate::inventory::{
    check_quantity_precision, check_stock_batch, check_stock_location, stock_bucket_quantity,
};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS stock_journals(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            date TEXT NOT NULL,
            voucher_type TEXT NOT NULL,
            reference TEXT NOT NULL,
            type_key TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            narration TEXT NOT NULL,
            UNIQUE(company_id, date, type_key, reference_key)
        );
        CREATE INDEX IF NOT EXISTS stock_journals_company_date ON stock_journals(company_id, date, id);
        CREATE TABLE IF NOT EXISTS stock_journal_entries(
            id INTEGER PRIMARY KEY,
            journal_id INTEGER NOT NULL REFERENCES stock_journals(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            quantity INTEGER NOT NULL CHECK(quantity != 0),
            rate TEXT,
            amount INTEGER CHECK(amount >= 0)
        );
        CREATE INDEX IF NOT EXISTS stock_journal_entries_journal ON stock_journal_entries(journal_id, id);
        CREATE TABLE IF NOT EXISTS stock_journal_allocations(
            id INTEGER PRIMARY KEY,
            entry_id INTEGER NOT NULL REFERENCES stock_journal_entries(id),
            adjustment_id INTEGER NOT NULL UNIQUE REFERENCES stock_adjustments(id),
            rate TEXT,
            amount INTEGER CHECK(amount >= 0)
        );
        CREATE INDEX IF NOT EXISTS stock_journal_allocations_entry ON stock_journal_allocations(entry_id, id);
        DROP TRIGGER IF EXISTS period_lock_stock_journals_INSERT;
        DROP TRIGGER IF EXISTS period_lock_stock_journals_UPDATE;
        DROP TRIGGER IF EXISTS period_lock_stock_journals_DELETE;",
    )?;
    crate::periods::protect_dated_table(conn, "stock_journals")
}

#[derive(Debug, Clone)]
pub struct StockJournal {
    pub id: i64,
    pub date: String,
    pub voucher_type: String,
    pub reference: String,
    pub narration: String,
}

#[derive(Debug, Clone)]
pub struct StockJournalEntry {
    pub id: i64,
    pub item: String,
    pub unit: String,
    pub quantity: i64,
    pub rate: Option<String>,
    pub amount: Option<i64>,
    pub allocations: Vec<StockJournalAllocation>,
}

#[derive(Debug, Clone)]
pub struct StockJournalAllocation {
    pub location: String,
    pub batch: Option<String>,
    pub quantity: i64,
    pub rate: Option<String>,
    pub amount: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StockJournalKind {
    #[default]
    Stock,
    Manufacturing,
}

impl StockJournalKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stock => "Stock Journal",
            Self::Manufacturing => "Manufacturing Journal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockJournalSide {
    Source,
    Destination,
}

impl StockJournalSide {
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Source",
            Self::Destination => "Destination",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StockJournalDraft {
    pub kind: StockJournalKind,
    pub date: String,
    pub reference: String,
    pub narration: String,
    pub lines: Vec<StockJournalLineDraft>,
}

/// Positive quantities in thousandths of the stock unit. Values use hundredths.
#[derive(Debug, Clone)]
pub struct StockJournalLineDraft {
    pub side: StockJournalSide,
    pub item_id: i64,
    pub location_id: i64,
    pub batch_id: Option<i64>,
    pub quantity: i64,
    pub amount: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockJournalBalance {
    pub item_id: i64,
    pub location_id: i64,
    pub batch_id: Option<i64>,
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub before: i128,
    pub issued: i128,
    pub received: i128,
}

impl StockJournalBalance {
    pub fn after(&self) -> i128 {
        self.before - self.issued + self.received
    }
}

#[derive(Debug, Clone)]
pub struct StockJournalPreview {
    company: i64,
    date: NaiveDate,
    draft: StockJournalDraft,
    balances: Vec<StockJournalBalance>,
}

impl StockJournalPreview {
    pub fn draft(&self) -> &StockJournalDraft {
        &self.draft
    }

    pub fn balances(&self) -> &[StockJournalBalance] {
        &self.balances
    }
}

pub(crate) struct JournalInput<'a> {
    pub date: &'a str,
    pub voucher_type: &'a str,
    pub reference: &'a str,
    pub narration: &'a str,
    pub entries: Vec<EntryInput<'a>>,
}

pub(crate) struct EntryInput<'a> {
    pub item: i64,
    pub quantity: i64,
    pub rate: Option<&'a str>,
    pub amount: Option<i64>,
    pub allocations: Vec<AllocationInput<'a>>,
}

pub(crate) struct AllocationInput<'a> {
    pub location: i64,
    pub batch: Option<i64>,
    pub quantity: i64,
    pub rate: Option<&'a str>,
    pub amount: Option<i64>,
}

pub(crate) fn write(conn: &Connection, company: i64, journal: JournalInput<'_>) -> Result<i64> {
    parse_date(journal.date)?;
    let inserted = conn.execute(
        "INSERT INTO stock_journals(company_id, date, voucher_type, reference, type_key, reference_key, narration)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(company_id, date, type_key, reference_key) DO NOTHING",
        params![
            company,
            journal.date,
            journal.voucher_type,
            journal.reference,
            journal.voucher_type.to_lowercase(),
            journal.reference.to_lowercase(),
            journal.narration,
        ],
    )?;
    if inserted == 0 {
        return Err(Error::Validation(format!(
            "Stock journal {} already exists on {}",
            journal.reference, journal.date
        )));
    }
    let journal_id = conn.last_insert_rowid();
    let reason = format!("{} {}", journal.voucher_type, journal.reference);
    let mut outgoing = BTreeSet::new();
    for entry in journal.entries {
        conn.execute(
            "INSERT INTO stock_journal_entries(journal_id, item_id, quantity, rate, amount)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                journal_id,
                entry.item,
                entry.quantity,
                entry.rate,
                entry.amount
            ],
        )?;
        let entry_id = conn.last_insert_rowid();
        for allocation in entry.allocations {
            let adjustment = crate::inventory::insert_stock_adjustment(
                conn,
                company,
                entry.item,
                allocation.location,
                allocation.batch,
                journal.date,
                allocation.quantity,
                &reason,
            )?;
            conn.execute(
                "INSERT INTO stock_journal_allocations(entry_id, adjustment_id, rate, amount)
                 VALUES (?1, ?2, ?3, ?4)",
                params![entry_id, adjustment, allocation.rate, allocation.amount],
            )?;
            if allocation.quantity < 0 {
                outgoing.insert((entry.item, allocation.location, allocation.batch));
            }
        }
    }
    for (item, location, batch) in outgoing {
        crate::stock_reservations::check_stock_coverage(conn, company, item, location, batch)?;
    }
    Ok(journal_id)
}

fn read_draft_balances(
    conn: &Connection,
    company: i64,
    date: NaiveDate,
    draft: &StockJournalDraft,
) -> Result<Vec<StockJournalBalance>> {
    if draft.lines.is_empty() {
        return Err(Error::Validation("Add a source or destination line".into()));
    }
    if crate::periods::cutoff(conn, company)?.is_some_and(|cutoff| date <= cutoff) {
        return Err(Error::Validation(
            crate::periods::LOCKED_PERIOD_MESSAGE.into(),
        ));
    }
    let mut movements = BTreeMap::<(i64, i64, Option<i64>), (i128, i128)>::new();
    for line in &draft.lines {
        if line.quantity <= 0 {
            return Err(Error::Validation(
                "Enter positive quantities on stock journal lines".into(),
            ));
        }
        if line.amount.is_some_and(|amount| amount < 0) {
            return Err(Error::Validation(
                "Recorded stock values must be non-negative".into(),
            ));
        }
        check_quantity_precision(conn, company, line.item_id, line.quantity)?;
        check_stock_location(conn, company, line.location_id)?;
        check_stock_batch(conn, company, line.item_id, line.batch_id)?;
        let (issued, received) = movements
            .entry((line.item_id, line.location_id, line.batch_id))
            .or_default();
        match line.side {
            StockJournalSide::Source => *issued += i128::from(line.quantity),
            StockJournalSide::Destination => *received += i128::from(line.quantity),
        }
    }
    let mut names = conn.prepare(
        "SELECT i.name, i.unit, l.name, b.name FROM stock_items i
         JOIN stock_locations l ON l.id = ?2 AND l.company_id = i.company_id
         LEFT JOIN stock_batches b ON b.id = ?3
         WHERE i.id = ?1 AND i.company_id = ?4",
    )?;
    movements
        .into_iter()
        .map(|((item_id, location_id, batch_id), (issued, received))| {
            let (item, unit, location, batch) = names
                .query_row(params![item_id, location_id, batch_id, company], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
            Ok(StockJournalBalance {
                item_id,
                location_id,
                batch_id,
                item,
                unit,
                location,
                batch,
                before: stock_bucket_quantity(
                    conn,
                    company,
                    item_id,
                    batch_id,
                    Some(location_id),
                    Some(date),
                )?,
                issued,
                received,
            })
        })
        .collect()
}

fn write_draft(
    conn: &Connection,
    company: i64,
    draft: &StockJournalDraft,
    balances: &[StockJournalBalance],
) -> Result<i64> {
    let entries = draft
        .lines
        .iter()
        .map(|line| {
            let quantity = match line.side {
                StockJournalSide::Source => -line.quantity,
                StockJournalSide::Destination => line.quantity,
            };
            EntryInput {
                item: line.item_id,
                quantity,
                rate: None,
                amount: line.amount,
                allocations: vec![AllocationInput {
                    location: line.location_id,
                    batch: line.batch_id,
                    quantity,
                    rate: None,
                    amount: None,
                }],
            }
        })
        .collect();
    let id = write(
        conn,
        company,
        JournalInput {
            date: &draft.date,
            voucher_type: draft.kind.label(),
            reference: &draft.reference,
            narration: &draft.narration,
            entries,
        },
    )?;
    for row in balances.iter().filter(|row| row.issued > row.received) {
        crate::stock_policy::check_bucket(
            conn,
            company,
            row.item_id,
            row.location_id,
            row.batch_id,
        )?;
    }
    Ok(id)
}

impl Store {
    /// Review the dated movements and validate the same writes that posting will perform.
    pub fn preview_stock_journal(
        &self,
        company: i64,
        draft: &StockJournalDraft,
    ) -> Result<StockJournalPreview> {
        let date = parse_date(&draft.date)?;
        let mut draft = draft.clone();
        draft.date = date.to_string();
        draft.reference = required(&draft.reference, "Journal reference")?.to_owned();
        draft.narration = draft.narration.trim().to_owned();
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let balances = read_draft_balances(&tx, company, date, &draft)?;
        write_draft(&tx, company, &draft, &balances)?;
        tx.rollback()?;
        Ok(StockJournalPreview {
            company,
            date,
            draft,
            balances,
        })
    }

    pub fn post_stock_journal(
        &mut self,
        company: i64,
        preview: &StockJournalPreview,
    ) -> Result<i64> {
        if company != preview.company {
            return Err(Error::Validation(
                "Preview the journal for the selected company first".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balances = read_draft_balances(&tx, company, preview.date, &preview.draft)?;
        if balances != preview.balances {
            return Err(Error::Validation(
                "Book quantities or stock details changed. Preview the journal again.".into(),
            ));
        }
        let id = write_draft(&tx, company, &preview.draft, &balances)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn stock_journals(&self, company: i64) -> Result<Vec<StockJournal>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, date, voucher_type, reference, narration FROM stock_journals
             WHERE company_id = ?1 ORDER BY date DESC, id DESC",
        )?;
        let rows = stmt.query_map([company], |row| {
            Ok(StockJournal {
                id: row.get(0)?,
                date: row.get(1)?,
                voucher_type: row.get(2)?,
                reference: row.get(3)?,
                narration: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn stock_journal_entries(
        &self,
        company: i64,
        journal: i64,
    ) -> Result<Vec<StockJournalEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, i.name, i.unit, e.quantity, e.rate, e.amount
             FROM stock_journal_entries e
             JOIN stock_journals j ON j.id = e.journal_id
             JOIN stock_items i ON i.id = e.item_id
             WHERE j.company_id = ?1 AND j.id = ?2 ORDER BY e.id",
        )?;
        let mut entries = stmt
            .query_map(params![company, journal], |row| {
                Ok(StockJournalEntry {
                    id: row.get(0)?,
                    item: row.get(1)?,
                    unit: row.get(2)?,
                    quantity: row.get(3)?,
                    rate: row.get(4)?,
                    amount: row.get(5)?,
                    allocations: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut allocations = self.conn.prepare(
            "SELECT l.name, b.name, a.quantity, s.rate, s.amount
             FROM stock_journal_allocations s
             JOIN stock_adjustments a ON a.id = s.adjustment_id
             JOIN stock_locations l ON l.id = a.location_id
             LEFT JOIN stock_batches b ON b.id = a.batch_id
             WHERE s.entry_id = ?1 ORDER BY s.id",
        )?;
        for entry in &mut entries {
            entry.allocations = allocations
                .query_map([entry.id], |row| {
                    Ok(StockJournalAllocation {
                        location: row.get(0)?,
                        batch: row.get(1)?,
                        quantity: row.get(2)?,
                        rate: row.get(3)?,
                        amount: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actual_cost::ActualCostMethod;
    use crate::inventory::InvoiceKind;
    use crate::orders::{OrderDraft, OrderLine};
    use crate::stock_reservations::StockReservationDraft;
    use crate::Category;

    #[test]
    fn native_journal_posts_consumption_and_multiple_valued_outputs_atomically() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Assembly").unwrap();
        let other_company = store.create_company("Other").unwrap();
        let main = store.stock_locations(company).unwrap()[0].id;
        let west = store.create_stock_location(company, "West").unwrap();
        let raw = store.create_stock_item(company, "Raw", "pcs").unwrap();
        let assembly = store.create_stock_item(company, "Assembly", "pcs").unwrap();
        let spare = store.create_stock_item(company, "Spare", "pcs").unwrap();
        let lot = store
            .create_stock_batch(company, assembly, "Lot A", None, None)
            .unwrap();
        let opening = store
            .adjust_stock_at(company, raw, main, "2026-04-01", 5000, "Opening")
            .unwrap();
        store
            .set_stock_receipt_cost(company, opening, 2500, "Opening value")
            .unwrap();
        let draft = StockJournalDraft {
            kind: StockJournalKind::Manufacturing,
            date: "2026-04-02".into(),
            reference: " PROD-1 ".into(),
            narration: "Assembly and spare output".into(),
            lines: vec![
                StockJournalLineDraft {
                    side: StockJournalSide::Source,
                    item_id: raw,
                    location_id: main,
                    batch_id: None,
                    quantity: 3000,
                    amount: None,
                },
                StockJournalLineDraft {
                    side: StockJournalSide::Destination,
                    item_id: assembly,
                    location_id: west,
                    batch_id: Some(lot),
                    quantity: 2000,
                    amount: Some(1500),
                },
                StockJournalLineDraft {
                    side: StockJournalSide::Destination,
                    item_id: spare,
                    location_id: west,
                    batch_id: None,
                    quantity: 1000,
                    amount: Some(0),
                },
            ],
        };
        let preview = store.preview_stock_journal(company, &draft).unwrap();
        assert_eq!(preview.draft().reference, "PROD-1");
        assert_eq!(preview.balances().len(), 3);
        let raw_balance = preview
            .balances()
            .iter()
            .find(|row| row.item_id == raw)
            .unwrap();
        assert_eq!((raw_balance.before, raw_balance.after()), (5000, 2000));
        assert!(store.stock_journals(company).unwrap().is_empty());
        assert!(store.post_stock_journal(other_company, &preview).is_err());
        let id = store.post_stock_journal(company, &preview).unwrap();
        let saved = store.stock_journals(company).unwrap().remove(0);
        assert_eq!(saved.voucher_type, "Manufacturing Journal");
        assert_eq!(saved.narration, "Assembly and spare output");
        let entries = store.stock_journal_entries(company, id).unwrap();
        assert_eq!(
            entries.iter().map(|row| row.quantity).collect::<Vec<_>>(),
            [-3000, 2000, 1000]
        );
        assert_eq!(entries[1].allocations[0].location, "West");
        assert_eq!(entries[1].allocations[0].batch.as_deref(), Some("Lot A"));
        assert_eq!(entries[1].amount, Some(1500));
        assert_eq!(entries[2].amount, Some(0));
        let valuation = store
            .stock_valuation_at_actual_cost(company, "2026-04-02", ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!((valuation.known_value, valuation.unavailable), (2500, 0));
        assert!(store.vouchers(company).unwrap().is_empty());
        assert!(store.post_stock_journal(company, &preview).is_err());
        assert_eq!(store.stock_journals(company).unwrap().len(), 1);

        let future = StockJournalDraft {
            date: "+12026-04-03".into(),
            reference: "PROD-FUTURE".into(),
            lines: vec![draft.lines[1].clone()],
            ..draft
        };
        let preview = store.preview_stock_journal(company, &future).unwrap();
        store.post_stock_journal(company, &preview).unwrap();
        let valuation = store
            .stock_valuation_at_actual_cost(company, "2026-04-02", ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!((valuation.known_value, valuation.unavailable), (2500, 0));
    }

    #[test]
    fn native_post_rechecks_stock_reservations_and_period_locks() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Warehouse").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let main = store.stock_locations(company).unwrap()[0].id;
        let west = store.create_stock_location(company, "West").unwrap();
        let date: String = store
            .conn
            .query_row("SELECT date('now','localtime')", [], |r| r.get(0))
            .unwrap();
        store
            .adjust_stock_at(company, item, main, &date, 5000, "Opening")
            .unwrap();
        store
            .set_negative_stock_policy(company, Some(&date), "Warehouse policy")
            .unwrap();
        let mut draft = StockJournalDraft {
            kind: StockJournalKind::Stock,
            date: date.clone(),
            reference: "MOVE-1".into(),
            narration: String::new(),
            lines: vec![
                StockJournalLineDraft {
                    side: StockJournalSide::Source,
                    item_id: item,
                    location_id: main,
                    batch_id: None,
                    quantity: 6000,
                    amount: None,
                },
                StockJournalLineDraft {
                    side: StockJournalSide::Destination,
                    item_id: item,
                    location_id: west,
                    batch_id: None,
                    quantity: 2000,
                    amount: None,
                },
            ],
        };
        let error = store
            .preview_stock_journal(company, &draft)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Negative stock is blocked"), "{error}");
        draft.lines[0].quantity = 2000;
        let preview = store.preview_stock_journal(company, &draft).unwrap();
        let order = store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Sale,
                    party_ledger_id: customer,
                    reference: "ORDER-1".into(),
                    date: date.clone(),
                    expected_on: None,
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: item,
                        quantity: 4000,
                        unit_price: 100,
                    }],
                },
            )
            .unwrap();
        let reservation = store
            .reserve_order_stock(
                company,
                order,
                &StockReservationDraft {
                    item_id: item,
                    location_id: main,
                    batch_id: None,
                    quantity: 4000,
                    reason: "Customer order".into(),
                },
            )
            .unwrap();
        let error = store
            .post_stock_journal(company, &preview)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Insufficient unreserved stock"), "{error}");
        store
            .release_order_stock(company, order, reservation, 4000, "Hold released")
            .unwrap();
        store
            .adjust_stock_at(company, item, main, &date, 1000, "Late receipt")
            .unwrap();
        let error = store
            .post_stock_journal(company, &preview)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Preview the journal again"), "{error}");
        let preview = store.preview_stock_journal(company, &draft).unwrap();
        store
            .set_period_lock(company, Some(&date), "Closed period")
            .unwrap();
        let error = store
            .post_stock_journal(company, &preview)
            .unwrap_err()
            .to_string();
        assert!(error.contains("locked period"), "{error}");
        assert!(store.stock_journals(company).unwrap().is_empty());
        assert_eq!(
            store
                .stock_register_at(company, item, Some(main), None, None)
                .unwrap()
                .closing_quantity,
            6000
        );
        assert_eq!(
            store
                .stock_register_at(company, item, Some(west), None, None)
                .unwrap()
                .closing_quantity,
            0
        );
    }
}
