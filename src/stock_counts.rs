//! Physical counts compare counted units with dated stock movements.
use crate::inventory::{check_quantity_precision, check_stock_batch, stock_bucket_quantity};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashSet;

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
    item_id: i64,
    batch_id: Option<i64>,
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

impl Store {
    pub fn preview_stock_count(
        &self,
        company: i64,
        draft: &StockCountDraft,
    ) -> Result<StockCountPlan> {
        let date = parse_date(&draft.date)?;
        let reference = required(&draft.reference, "Count reference")?;
        if draft.lines.is_empty() {
            return Err(Error::Validation("Enter at least one counted item".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        check_count_date(&tx, company, date)?;
        let location = tx
            .query_row(
                "SELECT name FROM stock_locations WHERE id = ?1 AND company_id = ?2",
                params![draft.location_id, company],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Location not found in this company".into()))?;
        let mut buckets = HashSet::new();
        let mut lines = Vec::with_capacity(draft.lines.len());
        for line in &draft.lines {
            if line.counted_quantity < 0 {
                return Err(Error::Validation(
                    "Counted quantities must be zero or positive".into(),
                ));
            }
            if !buckets.insert((line.item_id, line.batch_id)) {
                return Err(Error::Validation(
                    "Enter each item and batch combination once".into(),
                ));
            }
            check_quantity_precision(&tx, company, line.item_id, line.counted_quantity)?;
            check_stock_batch(&tx, company, line.item_id, line.batch_id)?;
            let (item, unit, batch) = tx.query_row(
                "SELECT s.name, s.unit, b.name FROM stock_items s
                 LEFT JOIN stock_batches b ON b.id = ?2 WHERE s.id = ?1",
                params![line.item_id, line.batch_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let book_quantity = book_quantity(
                &tx,
                company,
                line.item_id,
                line.batch_id,
                draft.location_id,
                date,
            )?;
            let row = StockCountLine {
                item_id: line.item_id,
                batch_id: line.batch_id,
                item,
                unit,
                batch,
                book_quantity,
                counted_quantity: line.counted_quantity,
                adjustment_id: None,
            };
            let variance = i64::try_from(row.variance())
                .map_err(|_| Error::Validation("Stock variance is too large".into()))?;
            check_quantity_precision(&tx, company, line.item_id, variance)?;
            lines.push(row);
        }
        tx.commit()?;
        Ok(StockCountPlan {
            company,
            location_id: draft.location_id,
            location,
            date,
            reference: reference.to_owned(),
            notes: draft.notes.trim().to_owned(),
            lines,
        })
    }

    /// Save the count and its quantity adjustments together if the preview is current.
    pub fn post_stock_count(&mut self, company: i64, plan: &StockCountPlan) -> Result<i64> {
        if plan.company != company {
            return Err(Error::Validation(
                "Preview the count for the selected company first".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_count_date(&tx, company, plan.date)?;
        let date = plan.date.format("%Y-%m-%d").to_string();
        let inserted = tx.execute(
            "INSERT INTO stock_counts(company_id, location_id, date, reference, reference_key, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(company_id, reference_key) DO NOTHING",
            params![company, plan.location_id, date, plan.reference, plan.reference.to_lowercase(), plan.notes],
        )?;
        if inserted == 0 {
            return Err(Error::Validation(
                "A stock count already uses this reference".into(),
            ));
        }
        let count = tx.last_insert_rowid();
        let reason = format!("Stock count {}", plan.reference);
        for row in &plan.lines {
            let current = book_quantity(
                &tx,
                company,
                row.item_id,
                row.batch_id,
                plan.location_id,
                plan.date,
            )?;
            if current != row.book_quantity {
                return Err(Error::Validation(format!(
                    "Book quantity for {} changed. Preview the count again.",
                    row.item
                )));
            }
            let variance = i64::try_from(row.variance())
                .map_err(|_| Error::Validation("Stock variance is too large".into()))?;
            let adjustment = if variance == 0 {
                None
            } else {
                tx.execute(
                    "INSERT INTO stock_adjustments(company_id, item_id, date, quantity, reason, location_id, batch_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![company, row.item_id, date, variance, reason, plan.location_id, row.batch_id],
                )?;
                Some(tx.last_insert_rowid())
            };
            tx.execute(
                "INSERT INTO stock_count_lines(stock_count_id, item_id, batch_id, book_quantity, counted_quantity, adjustment_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![count, row.item_id, row.batch_id, row.book_quantity, row.counted_quantity, adjustment],
            )?;
        }
        tx.commit()?;
        Ok(count)
    }

    pub fn stock_counts(&self, company: i64) -> Result<Vec<StockCountSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.date, c.reference, l.name, c.recorded_at, c.notes
             FROM stock_counts c JOIN stock_locations l ON l.id = c.location_id
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
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn stock_count_lines(&self, company: i64, count: i64) -> Result<Vec<StockCountLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, l.batch_id, s.name, s.unit, b.name, l.book_quantity, l.counted_quantity,
                    l.adjustment_id
             FROM stock_count_lines l JOIN stock_counts c ON c.id = l.stock_count_id
             JOIN stock_items s ON s.id = l.item_id
             LEFT JOIN stock_batches b ON b.id = l.batch_id
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
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
