//! Fixed monthly journal schedules with explicitly posted or skipped occurrences.

use crate::{
    check_ledgers, insert_voucher, opening, parse_date, required, validate_voucher, Error, Posting,
    Result, Store, VoucherKind,
};
use chrono::{Datelike, Months, NaiveDate};
use rusqlite::{params, Connection, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE recurring_journals(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            name TEXT NOT NULL COLLATE NOCASE,
            first_date TEXT NOT NULL,
            narration TEXT NOT NULL,
            closed_at TEXT,
            close_reason TEXT,
            CHECK((closed_at IS NULL) = (close_reason IS NULL))
        );
        CREATE UNIQUE INDEX recurring_journals_active_name
            ON recurring_journals(company_id, name) WHERE closed_at IS NULL;
        CREATE INDEX recurring_journals_company ON recurring_journals(company_id, id);
        CREATE TABLE recurring_journal_lines(
            journal_id INTEGER NOT NULL REFERENCES recurring_journals(id),
            line_number INTEGER NOT NULL,
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            amount INTEGER NOT NULL CHECK(amount != 0),
            PRIMARY KEY(journal_id, line_number)
        );
        CREATE TABLE recurring_journal_occurrences(
            journal_id INTEGER NOT NULL REFERENCES recurring_journals(id),
            month_number INTEGER NOT NULL CHECK(month_number >= 0),
            date TEXT NOT NULL,
            voucher_id INTEGER UNIQUE REFERENCES vouchers(id),
            skip_reason TEXT,
            resolved_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(journal_id, month_number),
            CHECK((voucher_id IS NULL) != (skip_reason IS NULL))
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct MonthlyJournal {
    pub id: i64,
    pub name: String,
    pub first_date: NaiveDate,
    pub narration: String,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
    next_month: u32,
}

impl MonthlyJournal {
    pub fn next_date(&self) -> Option<NaiveDate> {
        if self.closed_at.is_some() {
            return None;
        }
        self.first_date
            .checked_add_months(Months::new(self.next_month))
            .filter(|date| date.year() <= 9999)
    }
}

#[derive(Debug, Clone)]
pub struct JournalOccurrence {
    pub date: String,
    pub voucher_id: Option<i64>,
    pub reversal_id: Option<i64>,
    pub skip_reason: Option<String>,
    pub resolved_at: String,
}

fn journals(conn: &Connection, company: i64, id: Option<i64>) -> Result<Vec<MonthlyJournal>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.name, j.first_date, j.narration, j.closed_at, j.close_reason,
                COALESCE((SELECT MAX(o.month_number) + 1 FROM recurring_journal_occurrences o
                          WHERE o.journal_id = j.id), 0)
         FROM recurring_journals j WHERE j.company_id = ?1 AND (?2 IS NULL OR j.id = ?2)
         ORDER BY j.closed_at IS NOT NULL, j.name, j.id DESC",
    )?;
    let rows = stmt
        .query_and_then(params![company, id], |row| {
            Ok(MonthlyJournal {
                id: row.get(0)?,
                name: row.get(1)?,
                first_date: parse_date(&row.get::<_, String>(2)?)?,
                narration: row.get(3)?,
                closed_at: row.get(4)?,
                close_reason: row.get(5)?,
                next_month: row.get(6)?,
            })
        })?
        .collect();
    rows
}

fn journal_lines(conn: &Connection, company: i64, journal: i64) -> Result<Vec<Posting>> {
    let mut stmt = conn.prepare(
        "SELECT l.ledger_id, l.amount FROM recurring_journal_lines l
         JOIN recurring_journals j ON j.id = l.journal_id
         WHERE j.company_id = ?1 AND j.id = ?2 ORDER BY l.line_number",
    )?;
    let rows = stmt
        .query_map(params![company, journal], |row| {
            Ok(Posting {
                ledger_id: row.get(0)?,
                amount: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn contains_voucher(conn: &Connection, company: i64, voucher: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM recurring_journal_occurrences o
            JOIN recurring_journals j ON j.id = o.journal_id
            WHERE j.company_id = ?1 AND o.voucher_id = ?2
         )",
        params![company, voucher],
        |row| row.get(0),
    )?)
}

impl Store {
    pub fn create_monthly_journal(
        &mut self,
        company: i64,
        name: &str,
        first_date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<i64> {
        let name = required(name, "Schedule name")?;
        let first_date = validate_voucher(first_date, postings)?;
        if !(1..=9999).contains(&first_date.year()) {
            return Err(Error::Validation(
                "First date must use a four-digit year from 0001 to 9999".into(),
            ));
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_ledgers(&tx, company, postings)?;
        opening::check_no_control_postings(&tx, company, postings)?;
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recurring_journals
                           WHERE company_id = ?1 AND name = ?2 AND closed_at IS NULL)",
            params![company, name],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(Error::Validation(
                "An active schedule already has this name".into(),
            ));
        }

        tx.execute(
            "INSERT INTO recurring_journals(company_id, name, first_date, narration)
             VALUES (?1, ?2, ?3, ?4)",
            params![company, name, first_date.to_string(), narration.trim()],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut insert = tx.prepare(
                "INSERT INTO recurring_journal_lines(journal_id, line_number, ledger_id, amount)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (index, posting) in postings.iter().enumerate() {
                insert.execute(params![id, index, posting.ledger_id, posting.amount])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn monthly_journals(&self, company: i64) -> Result<Vec<MonthlyJournal>> {
        journals(&self.conn, company, None)
    }

    pub fn monthly_journal_lines(&self, company: i64, journal: i64) -> Result<Vec<Posting>> {
        journal_lines(&self.conn, company, journal)
    }

    pub fn monthly_journal_history(
        &self,
        company: i64,
        journal: i64,
    ) -> Result<Vec<JournalOccurrence>> {
        let mut stmt = self.conn.prepare(
            "SELECT o.date, o.voucher_id, r.reversal_voucher_id, o.skip_reason, o.resolved_at
             FROM recurring_journal_occurrences o
             JOIN recurring_journals j ON j.id = o.journal_id
             LEFT JOIN voucher_reversals r ON r.original_voucher_id = o.voucher_id
             WHERE j.company_id = ?1 AND j.id = ?2 ORDER BY o.month_number DESC",
        )?;
        let rows = stmt
            .query_map(params![company, journal], |row| {
                Ok(JournalOccurrence {
                    date: row.get(0)?,
                    voucher_id: row.get(1)?,
                    reversal_id: row.get(2)?,
                    skip_reason: row.get(3)?,
                    resolved_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn post_monthly_journal(
        &mut self,
        company: i64,
        journal: i64,
        date: NaiveDate,
    ) -> Result<i64> {
        self.resolve_monthly_journal(company, journal, date, None)?
            .ok_or_else(|| Error::Validation("No voucher was posted".into()))
    }

    pub fn skip_monthly_journal(
        &mut self,
        company: i64,
        journal: i64,
        date: NaiveDate,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Skip reason")?;
        self.resolve_monthly_journal(company, journal, date, Some(reason))?;
        Ok(())
    }

    fn resolve_monthly_journal(
        &mut self,
        company: i64,
        id: i64,
        date: NaiveDate,
        skip_reason: Option<&str>,
    ) -> Result<Option<i64>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let journal = journals(&tx, company, Some(id))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                Error::Validation("Schedule not found in the selected company".into())
            })?;
        if journal.next_date() != Some(date) {
            return Err(Error::Validation(
                "The next occurrence changed. Review the schedule again".into(),
            ));
        }

        let date_text = date.to_string();
        let voucher_id = if skip_reason.is_none() {
            let postings = journal_lines(&tx, company, id)?;
            validate_voucher(&date_text, &postings)?;
            Some(insert_voucher(
                &tx,
                company,
                VoucherKind::Journal,
                &format!("MJ-{id}-{date_text}"),
                date,
                &journal.narration,
                &postings,
            )?)
        } else {
            None
        };
        tx.execute(
            "INSERT INTO recurring_journal_occurrences(journal_id, month_number, date, voucher_id, skip_reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, journal.next_month, date_text, voucher_id, skip_reason],
        )?;
        tx.commit()?;
        Ok(voucher_id)
    }

    pub fn close_monthly_journal(
        &mut self,
        company: i64,
        journal: i64,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Close reason")?;
        let changed = self.conn.execute(
            "UPDATE recurring_journals SET closed_at = CURRENT_TIMESTAMP, close_reason = ?1
             WHERE company_id = ?2 AND id = ?3 AND closed_at IS NULL",
            params![reason, company, journal],
        )?;
        if changed != 1 {
            return Err(Error::Validation("Select an active schedule".into()));
        }
        Ok(())
    }

    pub fn is_monthly_journal_voucher(&self, company: i64, voucher: i64) -> Result<bool> {
        contains_voucher(&self.conn, company, voucher)
    }
}
