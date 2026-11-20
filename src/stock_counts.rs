//! Physical counts compare counted units with dated stock movements.
use crate::inventory::{check_quantity_precision, check_stock_batch, stock_bucket_quantity};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE stock_counts(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            location_id INTEGER NOT NULL REFERENCES stock_locations(id),
            date TEXT NOT NULL,
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            notes TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(company_id, reference_key)
        );
        CREATE INDEX stock_counts_company_date ON stock_counts(company_id, date, id);
        CREATE TABLE stock_count_lines(
            id INTEGER PRIMARY KEY,
            stock_count_id INTEGER NOT NULL REFERENCES stock_counts(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            batch_id INTEGER REFERENCES stock_batches(id),
            book_quantity INTEGER NOT NULL,
            counted_quantity INTEGER NOT NULL CHECK(counted_quantity >= 0),
            adjustment_id INTEGER UNIQUE REFERENCES stock_adjustments(id),
            CHECK((book_quantity = counted_quantity) = (adjustment_id IS NULL))
        );
        CREATE UNIQUE INDEX stock_count_lines_bucket
            ON stock_count_lines(stock_count_id, item_id, COALESCE(batch_id, 0));",
    )?;
    crate::periods::protect_dated_table(conn, "stock_counts")
}

pub(crate) fn migrate_locations_and_modes(conn: &Connection) -> Result<()> {
    let has_mode: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stock_counts') WHERE name = 'adjusts_stock')",
        [], |row| row.get(0),
    )?;
    if !has_mode {
        conn.execute_batch(
            "ALTER TABLE stock_counts ADD COLUMN adjusts_stock INTEGER NOT NULL DEFAULT 1
                 CHECK(adjusts_stock IN (0, 1));",
        )?;
    }
    let has_location: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stock_count_lines') WHERE name = 'location_id')",
        [], |row| row.get(0),
    )?;
    if !has_location {
        conn.execute_batch(
            "CREATE TABLE stock_count_lines_new(
                id INTEGER PRIMARY KEY,
                stock_count_id INTEGER NOT NULL REFERENCES stock_counts(id),
                item_id INTEGER NOT NULL REFERENCES stock_items(id),
                location_id INTEGER NOT NULL REFERENCES stock_locations(id),
                batch_id INTEGER REFERENCES stock_batches(id),
                book_quantity INTEGER NOT NULL,
                counted_quantity INTEGER NOT NULL CHECK(counted_quantity >= 0),
                adjustment_id INTEGER UNIQUE REFERENCES stock_adjustments(id),
                CHECK(adjustment_id IS NULL OR book_quantity <> counted_quantity)
             );
             INSERT INTO stock_count_lines_new
             SELECT l.id, l.stock_count_id, l.item_id, c.location_id, l.batch_id,
                    l.book_quantity, l.counted_quantity, l.adjustment_id
             FROM stock_count_lines l JOIN stock_counts c ON c.id = l.stock_count_id;
             DROP TABLE stock_count_lines;
             ALTER TABLE stock_count_lines_new RENAME TO stock_count_lines;
             CREATE UNIQUE INDEX stock_count_lines_bucket
                ON stock_count_lines(stock_count_id, location_id, item_id, COALESCE(batch_id, 0));",
        )?;
    }
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS stock_count_line_adjustment
         BEFORE INSERT ON stock_count_lines
         WHEN (NEW.adjustment_id IS NOT NULL) <> (
             NEW.book_quantity <> NEW.counted_quantity AND
             (SELECT adjusts_stock FROM stock_counts WHERE id = NEW.stock_count_id) = 1
         )
         BEGIN
            SELECT RAISE(ABORT, 'Count adjustment does not agree with the count mode');
         END;",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StockCountMode {
    #[default]
    Reconcile,
    Observe,
}

impl StockCountMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reconcile => "Adjust stock",
            Self::Observe => "Observation only",
        }
    }

    fn adjusts_stock(self) -> bool {
        self == Self::Reconcile
    }
}

#[derive(Debug, Clone)]
pub struct CountLine {
    pub item_id: i64,
    pub batch_id: Option<i64>,
    pub counted_quantity: i64,
}

#[derive(Debug, Clone)]
pub struct StockCountDraft {
    pub location_id: i64,
    pub date: String,
    pub reference: String,
    pub notes: String,
    pub lines: Vec<CountLine>,
}

#[derive(Debug, Clone)]
pub struct StockCountLine {
    pub(crate) item_id: i64,
    pub(crate) batch_id: Option<i64>,
    pub(crate) location_id: i64,
    pub location: String,
    pub item: String,
    pub unit: String,
    pub batch: Option<String>,
    pub book_quantity: i64,
    pub counted_quantity: i64,
    pub adjustment_id: Option<i64>,
}
impl StockCountLine {
    pub fn variance(&self) -> i128 {
        i128::from(self.counted_quantity) - i128::from(self.book_quantity)
    }
}

#[derive(Debug, Clone)]
pub struct StockCountPlan {
    company: i64,
    location_id: i64,
    location: String,
    date: NaiveDate,
    reference: String,
    notes: String,
    mode: StockCountMode,
    lines: Vec<StockCountLine>,
}
impl StockCountPlan {
    pub fn location(&self) -> &str {
        &self.location
    }

    pub fn date(&self) -> NaiveDate {
        self.date
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }

    pub fn mode(&self) -> StockCountMode {
        self.mode
    }

    pub fn lines(&self) -> &[StockCountLine] {
        &self.lines
    }
}

#[derive(Debug, Clone)]
pub struct StockCountSummary {
    pub id: i64,
    pub date: String,
    pub reference: String,
    pub location: String,
    pub recorded_at: String,
    pub notes: String,
    pub mode: StockCountMode,
}

fn check_count_date(conn: &Connection, company: i64, date: NaiveDate) -> Result<()> {
    if crate::periods::cutoff(conn, company)?.is_some_and(|cutoff| date <= cutoff) {
        return Err(Error::Validation(
            crate::periods::LOCKED_PERIOD_MESSAGE.into(),
        ));
    }
    Ok(())
}

fn book_quantity(
    conn: &Connection,
    company: i64,
    item: i64,
    batch: Option<i64>,
    location: i64,
    date: NaiveDate,
) -> Result<i64> {
    let total = stock_bucket_quantity(conn, company, item, batch, Some(location), Some(date))?;
    i64::try_from(total)
        .map_err(|_| Error::Validation("Book quantity is too large for this count".into()))
}

pub(crate) struct CountInput<'a> {
    pub date: &'a str,
    pub reference: &'a str,
    pub notes: &'a str,
    pub mode: StockCountMode,
    pub lines: &'a [(i64, CountLine)],
}

pub(crate) fn prepare_stock_count(
    conn: &Connection,
    company: i64,
    draft: CountInput<'_>,
) -> Result<StockCountPlan> {
    let date = parse_date(draft.date)?;
    let reference = required(draft.reference, "Count reference")?;
    let location_id = draft
        .lines
        .first()
        .map(|(location, _)| *location)
        .ok_or_else(|| Error::Validation("Enter at least one counted item".into()))?;
    check_count_date(conn, company, date)?;
    let mut locations = BTreeMap::new();
    let mut buckets = HashSet::new();
    let mut lines = Vec::with_capacity(draft.lines.len());
    for (location_id, line) in draft.lines {
        let location = match locations.entry(*location_id) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                conn.query_row(
                    "SELECT name FROM stock_locations WHERE id = ?1 AND company_id = ?2",
                    params![location_id, company],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| Error::Validation("Location not found in this company".into()))?,
            ),
        };
        if line.counted_quantity < 0 {
            return Err(Error::Validation(
                "Counted quantities must be zero or positive".into(),
            ));
        }
        if !buckets.insert((*location_id, line.item_id, line.batch_id)) {
            return Err(Error::Validation(
                "Enter each item, location and batch combination once".into(),
            ));
        }
        check_quantity_precision(conn, company, line.item_id, line.counted_quantity)?;
        check_stock_batch(conn, company, line.item_id, line.batch_id)?;
        let (item, unit, batch) = conn.query_row(
            "SELECT s.name, s.unit, b.name FROM stock_items s
             LEFT JOIN stock_batches b ON b.id = ?2 WHERE s.id = ?1",
            params![line.item_id, line.batch_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let row = StockCountLine {
            item_id: line.item_id,
            batch_id: line.batch_id,
            location_id: *location_id,
            location: location.clone(),
            item,
            unit,
            batch,
            book_quantity: book_quantity(
                conn,
                company,
                line.item_id,
                line.batch_id,
                *location_id,
                date,
            )?,
            counted_quantity: line.counted_quantity,
            adjustment_id: None,
        };
        if draft.mode.adjusts_stock() {
            let variance = i64::try_from(row.variance())
                .map_err(|_| Error::Validation("Stock variance is too large".into()))?;
            check_quantity_precision(conn, company, line.item_id, variance)?;
        }
        lines.push(row);
    }
    Ok(StockCountPlan {
        company,
        location_id,
        location: locations.into_values().collect::<Vec<_>>().join(", "),
        date,
        reference: reference.to_owned(),
        notes: draft.notes.trim().to_owned(),
        mode: draft.mode,
        lines,
    })
}

pub(crate) fn write_stock_count(
    conn: &Connection,
    company: i64,
    plan: &StockCountPlan,
    imported: bool,
) -> Result<i64> {
    if plan.company != company {
        return Err(Error::Validation(
            "Preview the count for the selected company first".into(),
        ));
    }
    check_count_date(conn, company, plan.date)?;
    let date = plan.date.to_string();
    let reference_key = if imported {
        format!("\0stock count/{date}/{}", plan.reference.to_lowercase())
    } else {
        plan.reference.to_lowercase()
    };
    let inserted = conn.execute(
        "INSERT INTO stock_counts(company_id, location_id, date, reference, reference_key, notes, adjusts_stock)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(company_id, reference_key) DO NOTHING",
        params![company, plan.location_id, date, plan.reference, reference_key, plan.notes, plan.mode.adjusts_stock()],
    )?;
    if inserted == 0 {
        return Err(Error::Validation(
            "A stock count already uses this reference".into(),
        ));
    }
    let count = conn.last_insert_rowid();
    let reason = format!("Stock count {}", plan.reference);
    for row in &plan.lines {
        let current = book_quantity(
            conn,
            company,
            row.item_id,
            row.batch_id,
            row.location_id,
            plan.date,
        )?;
        if current != row.book_quantity {
            return Err(Error::Validation(format!(
                "Book quantity for {} changed. Preview the count again.",
                row.item
            )));
        }
        let adjustment = if plan.mode.adjusts_stock() && row.variance() != 0 {
            let variance = i64::try_from(row.variance())
                .map_err(|_| Error::Validation("Stock variance is too large".into()))?;
            Some(crate::inventory::record_stock_adjustment(
                conn,
                company,
                row.item_id,
                row.location_id,
                row.batch_id,
                &date,
                variance,
                &reason,
            )?)
        } else {
            None
        };
        conn.execute(
            "INSERT INTO stock_count_lines(stock_count_id, item_id, location_id, batch_id, book_quantity, counted_quantity, adjustment_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![count, row.item_id, row.location_id, row.batch_id, row.book_quantity, row.counted_quantity, adjustment],
        )?;
    }
    Ok(count)
}

impl Store {
    pub fn preview_stock_count(
        &self,
        company: i64,
        draft: &StockCountDraft,
    ) -> Result<StockCountPlan> {
        self.preview_stock_count_with_mode(company, draft, StockCountMode::Reconcile)
    }

    pub fn preview_stock_count_with_mode(
        &self,
        company: i64,
        draft: &StockCountDraft,
        mode: StockCountMode,
    ) -> Result<StockCountPlan> {
        let tx = self.conn.unchecked_transaction()?;
        let lines = draft
            .lines
            .iter()
            .cloned()
            .map(|line| (draft.location_id, line))
            .collect::<Vec<_>>();
        let plan = prepare_stock_count(
            &tx,
            company,
            CountInput {
                date: &draft.date,
                reference: &draft.reference,
                notes: &draft.notes,
                mode,
                lines: &lines,
            },
        )?;
        tx.commit()?;
        Ok(plan)
    }

    /// Save the count and any quantity adjustments together if the preview is current.
    pub fn post_stock_count(&mut self, company: i64, plan: &StockCountPlan) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = write_stock_count(&tx, company, plan, false)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn stock_counts(&self, company: i64) -> Result<Vec<StockCountSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.date, c.reference, COALESCE((
                    SELECT GROUP_CONCAT(name, ', ') FROM (
                        SELECT DISTINCT l.name FROM stock_count_lines cl
                        JOIN stock_locations l ON l.id = cl.location_id
                        WHERE cl.stock_count_id = c.id ORDER BY l.name
                    )
                ), loc.name), c.recorded_at, c.notes, c.adjusts_stock
             FROM stock_counts c JOIN stock_locations loc ON loc.id = c.location_id
             WHERE c.company_id = ?1 ORDER BY c.date DESC, c.id DESC",
        )?;
        let rows = stmt.query_map([company], |row| {
            Ok(StockCountSummary {
                id: row.get(0)?,
                date: row.get(1)?,
                reference: row.get(2)?,
                location: row.get(3)?,
                recorded_at: row.get(4)?,
                notes: row.get(5)?,
                mode: if row.get(6)? {
                    StockCountMode::Reconcile
                } else {
                    StockCountMode::Observe
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn stock_count_lines(&self, company: i64, count: i64) -> Result<Vec<StockCountLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, l.batch_id, s.name, s.unit, b.name, l.book_quantity, l.counted_quantity,
                    l.adjustment_id, l.location_id, loc.name
             FROM stock_count_lines l JOIN stock_counts c ON c.id = l.stock_count_id
             JOIN stock_items s ON s.id = l.item_id
             LEFT JOIN stock_batches b ON b.id = l.batch_id
             JOIN stock_locations loc ON loc.id = l.location_id
             WHERE c.company_id = ?1 AND c.id = ?2 ORDER BY l.id",
        )?;
        let rows = stmt.query_map(params![company, count], |row| {
            Ok(StockCountLine {
                item_id: row.get(0)?,
                batch_id: row.get(1)?,
                item: row.get(2)?,
                unit: row.get(3)?,
                batch: row.get(4)?,
                book_quantity: row.get(5)?,
                counted_quantity: row.get(6)?,
                adjustment_id: row.get(7)?,
                location_id: row.get(8)?,
                location: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
