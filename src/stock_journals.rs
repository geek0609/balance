//! Dated inventory journals and their warehouse movements.
use crate::inventory::{
    check_quantity_precision, check_stock_batch, check_stock_location, stock_bucket_quantity,
};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::BTreeMap;

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

pub(crate) fn migrate_reversals(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS stock_journal_reversals (
            original_id INTEGER PRIMARY KEY REFERENCES stock_journals(id),
            reversal_id INTEGER NOT NULL UNIQUE REFERENCES stock_journals(id),
            CHECK(original_id != reversal_id)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct StockJournal {
    pub id: i64,
    pub date: String,
    pub voucher_type: String,
    pub reference: String,
    pub narration: String,
    pub reversal: Option<StockJournalLink>,
    pub reverses: Option<StockJournalLink>,
}

#[derive(Debug, Clone)]
pub struct StockJournalLink {
    pub id: i64,
    pub date: String,
    pub reference: String,
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
    /// Individual journals check the negative-stock policy after all their lines.
    /// Imports check it after the complete batch.
    pub enforce_negative_stock: bool,
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
    let mut changes = BTreeMap::<_, (i128, bool)>::new();
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
            let (quantity, has_issue) = changes
                .entry((entry.item, allocation.location, allocation.batch))
                .or_default();
            *quantity += i128::from(allocation.quantity);
            *has_issue |= allocation.quantity < 0;
        }
    }
    for ((item, location, batch), (quantity, has_issue)) in changes {
        if has_issue {
            crate::stock_reservations::check_stock_coverage(conn, company, item, location, batch)?;
        }
        if journal.enforce_negative_stock && quantity < 0 {
            crate::stock_policy::check_bucket(conn, company, item, location, batch)?;
        }
    }
    Ok(journal_id)
}

fn check_journal_date(conn: &Connection, company: i64, date: NaiveDate) -> Result<()> {
    if crate::periods::cutoff(conn, company)?.is_some_and(|cutoff| date <= cutoff) {
        return Err(Error::Validation(
            crate::periods::LOCKED_PERIOD_MESSAGE.into(),
        ));
    }
    Ok(())
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
    check_journal_date(conn, company, date)?;
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

fn write_draft(conn: &Connection, company: i64, draft: &StockJournalDraft) -> Result<i64> {
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
    write(
        conn,
        company,
        JournalInput {
            date: &draft.date,
            voucher_type: draft.kind.label(),
            reference: &draft.reference,
            narration: &draft.narration,
            entries,
            enforce_negative_stock: true,
        },
    )
}

fn reversed_entries(conn: &Connection, journal: i64) -> Result<Vec<EntryInput<'static>>> {
    let mut stmt = conn.prepare(
        "SELECT id, item_id, -quantity FROM stock_journal_entries
         WHERE journal_id = ?1 ORDER BY id",
    )?;
    let entries = stmt.query_map([journal], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            EntryInput {
                item: row.get(1)?,
                quantity: row.get(2)?,
                rate: None,
                amount: None,
                allocations: Vec::new(),
            },
        ))
    })?;
    let mut allocations = conn.prepare(
        "SELECT a.location_id, a.batch_id, -a.quantity
         FROM stock_journal_allocations s JOIN stock_adjustments a ON a.id = s.adjustment_id
         WHERE s.entry_id = ?1 ORDER BY s.id",
    )?;
    entries
        .map(|entry| {
            let (id, mut entry) = entry?;
            entry.allocations = allocations
                .query_map([id], |row| {
                    Ok(AllocationInput {
                        location: row.get(0)?,
                        batch: row.get(1)?,
                        quantity: row.get(2)?,
                        rate: None,
                        amount: None,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(entry)
        })
        .collect()
}

fn read_journal_link(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<StockJournalLink>> {
    row.get::<_, Option<i64>>(column)?
        .map(|id| {
            Ok(StockJournalLink {
                id,
                date: row.get(column + 1)?,
                reference: row.get(column + 2)?,
            })
        })
        .transpose()
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
        write_draft(&tx, company, &draft)?;
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
        let id = write_draft(&tx, company, &preview.draft)?;
        tx.commit()?;
        Ok(id)
    }

    /// Post one dated reversal and retain both journal records in inventory history.
    pub fn reverse_stock_journal(
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
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (original_date, voucher_type, linked): (String, String, bool) = tx.query_row(
            "SELECT j.date, j.voucher_type,
                    EXISTS(SELECT 1 FROM stock_journal_reversals r WHERE r.original_id = j.id OR r.reversal_id = j.id)
             FROM stock_journals j WHERE j.company_id = ?1 AND j.id = ?2",
            params![company, original],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Stock journal not found in the selected company".into()))?;
        if linked {
            return Err(Error::Validation(
                "This stock journal is already linked to a reversal".into(),
            ));
        }
        if date < parse_date(&original_date)? {
            return Err(Error::Validation(
                "Reversal date cannot precede the stock journal date".into(),
            ));
        }
        check_journal_date(&tx, company, date)?;
        let entries = reversed_entries(&tx, original)?;
        if entries.is_empty() {
            return Err(Error::Validation(
                "This stock journal has no stock lines".into(),
            ));
        }
        let reversal = write(
            &tx,
            company,
            JournalInput {
                date: &date.to_string(),
                voucher_type: &voucher_type,
                reference,
                narration: reason,
                entries,
                enforce_negative_stock: true,
            },
        )?;
        tx.execute(
            "INSERT INTO stock_journal_reversals(original_id, reversal_id) VALUES (?1, ?2)",
            params![original, reversal],
        )?;
        tx.commit()?;
        Ok(reversal)
    }

    pub fn stock_journals(&self, company: i64) -> Result<Vec<StockJournal>> {
        let mut stmt = self.conn.prepare(
            "SELECT j.id, j.date, j.voucher_type, j.reference, j.narration,
                    undo.id, undo.date, undo.reference, original.id, original.date, original.reference
             FROM stock_journals j
             LEFT JOIN stock_journal_reversals f ON f.original_id = j.id
             LEFT JOIN stock_journals undo ON undo.id = f.reversal_id
             LEFT JOIN stock_journal_reversals b ON b.reversal_id = j.id
             LEFT JOIN stock_journals original ON original.id = b.original_id
             WHERE j.company_id = ?1 ORDER BY j.date DESC, j.id DESC",
        )?;
        let rows = stmt.query_map([company], |row| {
            Ok(StockJournal {
                id: row.get(0)?,
                date: row.get(1)?,
                voucher_type: row.get(2)?,
                reference: row.get(3)?,
                narration: row.get(4)?,
                reversal: read_journal_link(row, 5)?,
                reverses: read_journal_link(row, 8)?,
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

    #[test]
    fn reverses_imported_allocations_and_restores_original_issue_costs() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Production").unwrap();
        let other = store.create_company("Other").unwrap();
        let main = store.stock_locations(company).unwrap()[0].id;
        let west = store.create_stock_location(company, "West").unwrap();
        let raw = store.create_stock_item(company, "Raw", "pcs").unwrap();
        let product = store.create_stock_item(company, "Product", "pcs").unwrap();
        let lot_a = store
            .create_stock_batch(company, raw, "A", None, None)
            .unwrap();
        let lot_b = store
            .create_stock_batch(company, raw, "B", None, None)
            .unwrap();
        for (day, lot, qty, amount) in [
            ("2026-04-01", lot_a, 1000, 200),
            ("2026-04-02", lot_b, 3000, 1500),
            ("2026-04-04", lot_b, 1000, 1000),
        ] {
            let receipt = store
                .adjust_stock_batch(company, raw, main, Some(lot), day, qty, "Raw stock")
                .unwrap();
            store
                .set_stock_receipt_cost(company, receipt, amount, "Receipt value")
                .unwrap();
        }
        let xml = br#"<ENVELOPE><BODY><DATA><MESSAGE>
          <VOUCHER><VOUCHERTYPENAME>Manufacturing Journal</VOUCHERTYPENAME>
            <DATE>20260403</DATE><VOUCHERNUMBER>PROD-1</VOUCHERNUMBER>
            <INVENTORYENTRIESOUT.LIST><STOCKITEMNAME>Raw</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
              <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><BATCHNAME>A</BATCHNAME>
                <ACTUALQTY>1 pcs</ACTUALQTY>
              </BATCHALLOCATIONS.LIST>
              <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><BATCHNAME>B</BATCHNAME>
                <ACTUALQTY>1 pcs</ACTUALQTY>
              </BATCHALLOCATIONS.LIST>
            </INVENTORYENTRIESOUT.LIST>
            <INVENTORYENTRIESIN.LIST><STOCKITEMNAME>Product</STOCKITEMNAME>
              <ACTUALQTY>1 pcs</ACTUALQTY><GODOWNNAME>West</GODOWNNAME><AMOUNT>12.00</AMOUNT>
            </INVENTORYENTRIESIN.LIST>
          </VOUCHER>
        </MESSAGE></DATA></BODY></ENVELOPE>"#;
        let preview = store.preview_xml_import(company, xml).unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        store.apply_xml_import(&preview).unwrap();
        let original = store.stock_journals(company).unwrap().remove(0).id;
        store
            .adjust_stock_batch(
                company,
                raw,
                main,
                Some(lot_b),
                "2026-04-05",
                -1000,
                "Other material issue",
            )
            .unwrap();
        store
            .adjust_stock_at(
                company,
                product,
                west,
                "2026-04-07",
                -1000,
                "Future stock issue",
            )
            .unwrap();
        store
            .set_negative_stock_policy(company, Some("2026-04-01"), "Stock policy")
            .unwrap();
        assert!(store
            .reverse_stock_journal(other, original, "2026-04-06", "REV-1", "Correction")
            .is_err());
        let error = store
            .reverse_stock_journal(company, original, "2026-04-02", "REV-1", "Correction")
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot precede"), "{error}");
        let error = store
            .reverse_stock_journal(company, original, "2026-04-06", "REV-1", "Correction")
            .unwrap_err()
            .to_string();
        assert!(error.contains("Negative stock is blocked"), "{error}");
        assert_eq!(store.stock_journals(company).unwrap().len(), 1);
        assert_eq!(
            store
                .stock_register_at(company, raw, Some(main), None, Some("2026-04-06"))
                .unwrap()
                .closing_quantity,
            2000
        );
        store
            .adjust_stock_at(
                company,
                product,
                west,
                "2026-04-07",
                1000,
                "Replacement stock",
            )
            .unwrap();
        store
            .set_period_lock(company, Some("2026-04-03"), "Closed production period")
            .unwrap();
        let error = store
            .reverse_stock_journal(company, original, "2026-04-03", "REV-1", "Correction")
            .unwrap_err()
            .to_string();
        assert!(error.contains("locked period"), "{error}");
        let reversal = store
            .reverse_stock_journal(company, original, "2026-04-06", "REV-1", "Correction")
            .unwrap();
        let history = store.stock_journals(company).unwrap();
        assert_eq!(history[0].reference, "REV-1");
        assert_eq!(history[0].narration, "Correction");
        assert_eq!(history[0].reverses.as_ref().unwrap().id, original);
        assert_eq!(history[1].reference, "PROD-1");
        assert_eq!(history[1].reversal.as_ref().unwrap().id, reversal);
        let entries = store.stock_journal_entries(company, reversal).unwrap();
        assert_eq!(entries[0].quantity, 2000);
        assert_eq!(entries[0].allocations[0].batch.as_deref(), Some("A"));
        assert_eq!(entries[0].allocations[1].batch.as_deref(), Some("B"));
        assert_eq!(entries[1].quantity, -1000);
        assert_eq!(entries[1].allocations[0].location, "West");
        assert!(store
            .reverse_stock_journal(company, original, "2026-04-07", "REV-2", "Again")
            .is_err());
        assert!(store
            .reverse_stock_journal(company, reversal, "2026-04-07", "REV-3", "Again")
            .is_err());
        for (method, value_before, value_after) in [
            (ActualCostMethod::Fifo, 2700, 2200),
            (ActualCostMethod::WeightedAverage, 2433, 2083),
        ] {
            let before = store
                .stock_valuation_at_actual_cost(company, "2026-04-05", method)
                .unwrap();
            assert_eq!((before.known_value, before.unavailable), (value_before, 0));
            let after = store
                .stock_valuation_at_actual_cost(company, "2026-04-06", method)
                .unwrap();
            assert_eq!((after.known_value, after.unavailable), (value_after, 0));
            let raw = after.rows.iter().find(|row| row.item.id == raw).unwrap();
            assert_eq!((raw.quantity, raw.value), (4000, Some(value_after)));
        }
    }

    #[test]
    fn same_day_reversal_cancels_even_an_uncosted_receipt() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Receipts").unwrap();
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let opening = store
            .adjust_stock_at(company, item, location, "2026-04-01", 2000, "Opening")
            .unwrap();
        store
            .set_stock_receipt_cost(company, opening, 300, "Opening value")
            .unwrap();
        let date = "2026-04-02";
        let draft = StockJournalDraft {
            kind: StockJournalKind::Stock,
            date: date.into(),
            reference: "RX-1".into(),
            narration: String::new(),
            lines: vec![StockJournalLineDraft {
                side: StockJournalSide::Destination,
                item_id: item,
                location_id: location,
                batch_id: None,
                quantity: 1000,
                amount: None,
            }],
        };
        let preview = store.preview_stock_journal(company, &draft).unwrap();
        let journal = store.post_stock_journal(company, &preview).unwrap();
        let before = store
            .stock_valuation_at_actual_cost(company, date, ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(before.unavailable, 1);
        store
            .reverse_stock_journal(company, journal, date, "RX-REV", "Entry cancelled")
            .unwrap();
        let after = store
            .stock_valuation_at_actual_cost(company, date, ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(
            (after.rows[0].quantity, after.known_value, after.unavailable),
            (2000, 300, 0)
        );
        let report = store.stock_ageing(company, date, [30, 60, 90]).unwrap();
        assert_eq!(report.rows[0].quantity, 2000);
        assert_eq!(
            report.rows[0].breakdown.as_ref().unwrap().fifo_value,
            Some(300)
        );
    }
}
