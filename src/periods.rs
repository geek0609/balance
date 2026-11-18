//! Company transaction cutoffs and their change history.
use crate::{parse_date, required, Error, Result, Store};
use chrono::{Datelike, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) const LOCKED_PERIOD_MESSAGE: &str =
    "Transaction date is in a locked period. See Companies for the cutoff.";

#[derive(Debug, Clone)]
pub struct PeriodLockEvent {
    pub locked_through: Option<NaiveDate>,
    pub changed_at: String,
    pub reason: String,
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE period_lock_events(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            locked_through TEXT,
            changed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            reason TEXT NOT NULL
        );
        CREATE INDEX period_lock_events_company ON period_lock_events(company_id, id);",
    )?;
    for table in ["vouchers", "stock_adjustments", "stock_transfers"] {
        protect_dated_table(conn, table)?;
    }
    Ok(())
}

pub(crate) fn protect_dated_table(conn: &Connection, table: &str) -> Result<()> {
    for (action, rows) in [
        ("INSERT", &["NEW"][..]),
        ("UPDATE", &["OLD", "NEW"][..]),
        ("DELETE", &["OLD"][..]),
    ] {
        // Transaction dates are canonical ISO strings. Chrono prefixes years
        // above 9999 with a plus sign, so those follow every valid cutoff.
        let conditions = rows
            .iter()
            .map(|row| {
                format!(
                    "({row}.date NOT GLOB '+*' AND {row}.date <= (
                        SELECT locked_through FROM period_lock_events
                        WHERE company_id = {row}.company_id ORDER BY id DESC LIMIT 1
                    ))"
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        conn.execute_batch(&format!(
            "CREATE TRIGGER period_lock_{table}_{action}
             BEFORE {action} ON {table}
             WHEN {conditions}
             BEGIN
                 SELECT RAISE(ABORT, '{LOCKED_PERIOD_MESSAGE}');
             END;"
        ))?;
    }
    Ok(())
}

pub(crate) fn cutoff(conn: &Connection, company: i64) -> Result<Option<NaiveDate>> {
    let cutoff: Option<Option<String>> = conn
        .query_row(
            "SELECT locked_through FROM period_lock_events
             WHERE company_id = ?1 ORDER BY id DESC LIMIT 1",
            [company],
            |row| row.get(0),
        )
        .optional()?;
    cutoff.flatten().as_deref().map(parse_date).transpose()
}

impl Store {
    pub fn period_lock(&self, company: i64) -> Result<Option<NaiveDate>> {
        cutoff(&self.conn, company)
    }

    /// The cutoff is inclusive. Passing no date reopens all periods.
    pub fn set_period_lock(
        &mut self,
        company: i64,
        through: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let requested = through.map(parse_date).transpose()?;
        if requested.is_some_and(|date| !(1..=9999).contains(&date.year())) {
            return Err(Error::Validation(
                "Lock date must use a four-digit year from 0001 to 9999".into(),
            ));
        }
        let reason = required(reason, "Reason for changing the period lock")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let company_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM companies WHERE id = ?1)",
            [company],
            |row| row.get(0),
        )?;
        if !company_exists {
            return Err(Error::Validation("Company not found".into()));
        }
        if requested == cutoff(&tx, company)? {
            return Err(Error::Validation(
                "The requested period lock is already in effect".into(),
            ));
        }
        let date = requested.map(|date| date.format("%Y-%m-%d").to_string());
        tx.execute(
            "INSERT INTO period_lock_events(company_id, locked_through, reason) VALUES (?1, ?2, ?3)",
            params![company, date, reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn period_lock_history(&self, company: i64) -> Result<Vec<PeriodLockEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT locked_through, changed_at, reason FROM period_lock_events
             WHERE company_id = ?1 ORDER BY id DESC",
        )?;
        let mut rows = stmt.query([company])?;
        let mut history = Vec::new();
        while let Some(row) = rows.next()? {
            let date: Option<String> = row.get(0)?;
            history.push(PeriodLockEvent {
                locked_through: date.as_deref().map(parse_date).transpose()?,
                changed_at: row.get(1)?,
                reason: row.get(2)?,
            });
        }
        Ok(history)
    }
}
