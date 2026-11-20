//! Dated inventory journals and their warehouse movements.
use crate::{parse_date, Error, Result, Store};
use rusqlite::{params, Connection};
use std::collections::HashSet;

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
    let mut outgoing = HashSet::new();
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

impl Store {
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
