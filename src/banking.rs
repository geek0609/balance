//! Manual clearance of cash and bank ledger postings.
use crate::{parse_amount, parse_date, required, Category, Error, Result, Store};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct BankEntry {
    pub posting_id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub narration: String,
    pub amount: i64,
    pub cleared_on: Option<String>,
}
#[derive(Debug, Clone)]
pub struct BankReport {
    pub entries: Vec<BankEntry>,
    pub book_balance: i128,
    pub cleared_balance: i128,
    pub uncleared: i128,
}
#[derive(Debug, Clone)]
pub struct ClearanceEvent {
    pub changed_at: String,
    pub posting_id: i64,
    pub ledger: String,
    pub amount: i64,
    pub voucher_date: String,
    pub previous_date: Option<String>,
    pub new_date: Option<String>,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct StatementRow {
    pub external_id: String,
    pub date: String,
    pub description: String,
    pub amount: i64,
}
#[derive(Debug, Clone)]
pub struct StatementPlan {
    company: i64,
    ledger: i64,
    rows: Vec<StatementRow>,
    issues: Vec<String>,
}
impl StatementPlan {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
    pub fn issues(&self) -> &[String] {
        &self.issues
    }
    pub fn rows(&self) -> &[StatementRow] {
        &self.rows
    }
    pub fn can_apply(&self) -> bool {
        !self.rows.is_empty() && self.issues.is_empty()
    }
    pub fn company(&self) -> i64 {
        self.company
    }
    pub fn ledger(&self) -> i64 {
        self.ledger
    }
}
#[derive(Debug, Clone)]
pub struct StatementEntry {
    pub id: i64,
    pub external_id: String,
    pub date: String,
    pub description: String,
    pub amount: i64,
    pub matched_posting: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct MatchEvent {
    pub changed_at: String,
    pub action: String,
    pub voucher_id: i64,
    pub posting_id: i64,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct StatementOpening {
    pub start_date: String,
    pub amount: i64,
}
#[derive(Debug, Clone)]
pub struct StatementBalance {
    pub opening: StatementOpening,
    pub movement_total: i128,
    pub ending_balance: i128,
    pub entry_count: usize,
}
#[derive(Debug, Clone)]
pub struct StatementOpeningEvent {
    pub changed_at: String,
    pub previous_date: Option<String>,
    pub previous_amount: Option<i64>,
    pub new_date: Option<String>,
    pub new_amount: Option<i64>,
    pub reason: String,
}
pub fn parse_signed_amount(value: &str) -> Result<i64> {
    let value = value.trim();
    if let Some(number) = value.strip_prefix('-') {
        parse_amount(number)?
            .checked_neg()
            .ok_or_else(|| Error::Validation("Amount is too large".into()))
    } else {
        parse_amount(value.strip_prefix('+').unwrap_or(value))
    }
}

pub(crate) fn ensure_no_bank_clearance(
    conn: &Connection,
    company: i64,
    voucher: i64,
) -> Result<()> {
    let cleared: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM bank_clearances c JOIN postings p ON p.id = c.posting_id WHERE c.company_id = ?1 AND p.voucher_id = ?2)", params![company, voucher], |r| r.get(0))?;
    if cleared {
        return Err(Error::Validation(
            "Remove the bank clearance before editing this voucher".into(),
        ));
    }
    Ok(())
}
fn asset_ledger(conn: &Connection, company: i64, ledger: i64) -> Result<()> {
    let category: Option<String> = conn
        .query_row(
            "SELECT category FROM ledgers WHERE id = ?1 AND company_id = ?2",
            params![ledger, company],
            |r| r.get(0),
        )
        .optional()?;
    if category.as_deref() != Some(Category::Asset.as_str()) {
        return Err(Error::Validation(
            "Select an asset ledger in the selected company".into(),
        ));
    }
    Ok(())
}
fn record_clearance(
    conn: &Connection,
    company: i64,
    ledger: i64,
    posting: i64,
    date: Option<&str>,
    reason: &str,
) -> Result<()> {
    let reason = required(reason, "Reason")?;
    let new_date = date
        .map(parse_date)
        .transpose()?
        .map(|d| d.format("%Y-%m-%d").to_string());
    asset_ledger(conn, company, ledger)?;
    let (voucher, voucher_date, amount): (i64, String, i64) = conn.query_row("SELECT v.id, v.date, p.amount FROM postings p JOIN vouchers v ON v.id = p.voucher_id WHERE p.id = ?1 AND p.ledger_id = ?2 AND v.company_id = ?3", params![posting, ledger, company], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Posting not found in this ledger and company".into()), other => Error::Database(other) })?;
    if let Some(new_date) = &new_date {
        if parse_date(new_date)? < parse_date(&voucher_date)? {
            return Err(Error::Validation(
                "Clearance date must not precede the voucher date".into(),
            ));
        }
    }
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT company_id, cleared_on FROM bank_clearances WHERE posting_id = ?1",
            [posting],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if existing
        .as_ref()
        .is_some_and(|(owner, _)| *owner != company)
    {
        return Err(Error::Validation(
            "Clearance has an inconsistent company".into(),
        ));
    }
    let previous = existing.map(|(_, date)| date);
    if previous == new_date {
        return Err(Error::Validation("Clearance has not changed".into()));
    }
    conn.execute("INSERT INTO bank_clearance_events(company_id, voucher_id, posting_id, ledger_id, amount, voucher_date, previous_date, new_date, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)", params![company, voucher, posting, ledger, amount, voucher_date, previous, new_date, reason])?;
    if let Some(value) = new_date {
        conn.execute("INSERT INTO bank_clearances(posting_id, company_id, cleared_on) VALUES (?1, ?2, ?3) ON CONFLICT(posting_id) DO UPDATE SET cleared_on = excluded.cleared_on", params![posting, company, value])?;
    } else {
        conn.execute(
            "DELETE FROM bank_clearances WHERE posting_id = ?1 AND company_id = ?2",
            params![posting, company],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn statement_opening(&self, company: i64, ledger: i64) -> Result<Option<StatementOpening>> {
        asset_ledger(&self.conn, company, ledger)?;
        Ok(self.conn.query_row("SELECT start_date, amount FROM bank_statement_openings WHERE company_id = ?1 AND ledger_id = ?2", params![company, ledger], |r| Ok(StatementOpening { start_date: r.get(0)?, amount: r.get(1)? })).optional()?)
    }
    /// Save or remove the balance immediately before the start of the selected date.
    pub fn set_statement_opening(
        &mut self,
        company: i64,
        ledger: i64,
        opening: Option<(&str, i64)>,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason")?;
        let next = opening
            .map(|(date, amount)| -> Result<_> {
                Ok((parse_date(date)?.format("%Y-%m-%d").to_string(), amount))
            })
            .transpose()?;
        let tx = self.conn.transaction()?;
        asset_ledger(&tx, company, ledger)?;
        let previous: Option<(String, i64)> = tx.query_row("SELECT start_date, amount FROM bank_statement_openings WHERE company_id = ?1 AND ledger_id = ?2", params![company, ledger], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if previous == next {
            return Err(Error::Validation(
                "Statement opening has not changed".into(),
            ));
        }
        tx.execute("INSERT INTO bank_statement_opening_events(company_id, ledger_id, previous_date, previous_amount, new_date, new_amount, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![company, ledger, previous.as_ref().map(|v| &v.0), previous.as_ref().map(|v| v.1), next.as_ref().map(|v| &v.0), next.as_ref().map(|v| v.1), reason])?;
        if let Some((date, amount)) = next {
            tx.execute("INSERT INTO bank_statement_openings(company_id, ledger_id, start_date, amount) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(company_id, ledger_id) DO UPDATE SET start_date = excluded.start_date, amount = excluded.amount", params![company, ledger, date, amount])?;
        } else {
            tx.execute(
                "DELETE FROM bank_statement_openings WHERE company_id = ?1 AND ledger_id = ?2",
                params![company, ledger],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn statement_balance(
        &self,
        company: i64,
        ledger: i64,
        as_of: &str,
    ) -> Result<Option<StatementBalance>> {
        let end = parse_date(as_of)?;
        let Some(opening) = self.statement_opening(company, ledger)? else {
            return Ok(None);
        };
        let start = parse_date(&opening.start_date)?;
        if end < start {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare("SELECT date, amount FROM bank_statement_entries WHERE company_id = ?1 AND ledger_id = ?2")?;
        let mut movement_total = 0_i128;
        let mut entry_count = 0_usize;
        for row in stmt.query_map(params![company, ledger], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (date, amount) = row?;
            let date = parse_date(&date)?;
            if date >= start && date <= end {
                movement_total += i128::from(amount);
                entry_count += 1;
            }
        }
        Ok(Some(StatementBalance {
            ending_balance: i128::from(opening.amount) + movement_total,
            opening,
            movement_total,
            entry_count,
        }))
    }
    pub fn statement_opening_events(
        &self,
        company: i64,
        ledger: i64,
    ) -> Result<Vec<StatementOpeningEvent>> {
        asset_ledger(&self.conn, company, ledger)?;
        let mut stmt = self.conn.prepare("SELECT changed_at, previous_date, previous_amount, new_date, new_amount, reason FROM bank_statement_opening_events WHERE company_id = ?1 AND ledger_id = ?2 ORDER BY id DESC")?;
        let rows = stmt
            .query_map(params![company, ledger], |r| {
                Ok(StatementOpeningEvent {
                    changed_at: r.get(0)?,
                    previous_date: r.get(1)?,
                    previous_amount: r.get(2)?,
                    new_date: r.get(3)?,
                    new_amount: r.get(4)?,
                    reason: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Preview a strict CSV with id,date,description,amount columns.
    pub fn preview_bank_statement(
        &self,
        company: i64,
        ledger: i64,
        bytes: &[u8],
    ) -> Result<StatementPlan> {
        asset_ledger(&self.conn, company, ledger)?;
        if bytes.len() > 10 * 1024 * 1024 {
            return Err(Error::Validation("Statement file exceeds 10 MiB".into()));
        }
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(false)
            .from_reader(bytes);
        let headers = reader
            .headers()
            .map_err(|e| Error::Validation(format!("Cannot read statement header: {e}")))?;
        if headers.len() != 4
            || !headers
                .iter()
                .zip(["id", "date", "description", "amount"])
                .all(|(actual, expected)| actual.trim().eq_ignore_ascii_case(expected))
        {
            return Err(Error::Validation(
                "Statement columns must be id,date,description,amount in that order".into(),
            ));
        }
        let mut seen = HashSet::new();
        let mut stmt = self.conn.prepare("SELECT external_id FROM bank_statement_entries WHERE company_id = ?1 AND ledger_id = ?2")?;
        for row in stmt.query_map(params![company, ledger], |r| r.get::<_, String>(0))? {
            seen.insert(row?.trim().to_lowercase());
        }
        let mut plan = StatementPlan {
            company,
            ledger,
            rows: Vec::new(),
            issues: Vec::new(),
        };
        for (index, record) in reader.records().enumerate() {
            if index >= 10_000 {
                return Err(Error::Validation(
                    "Statement has more than 10,000 rows".into(),
                ));
            }
            let record = record.map_err(|e| {
                Error::Validation(format!("Cannot read statement row {}: {e}", index + 2))
            })?;
            let id = record[0].trim().to_owned();
            if id.is_empty() {
                plan.issues
                    .push(format!("Row {}: id is required", index + 2));
                continue;
            }
            if !seen.insert(id.to_lowercase()) {
                plan.issues.push(format!(
                    "Row {}: id already exists or is repeated",
                    index + 2
                ));
            }
            let date = match parse_date(&record[1]) {
                Ok(date) => date.format("%Y-%m-%d").to_string(),
                Err(_) => {
                    plan.issues.push(format!("Row {}: invalid date", index + 2));
                    continue;
                }
            };
            let value = record[3].trim();
            let amount = if let Some(number) = value.strip_prefix('-') {
                parse_amount(number).and_then(|v| {
                    v.checked_neg()
                        .ok_or_else(|| Error::Validation("Amount is too large".into()))
                })
            } else {
                parse_amount(value.strip_prefix('+').unwrap_or(value))
            };
            let amount = match amount {
                Ok(amount) if amount != 0 => amount,
                _ => {
                    plan.issues
                        .push(format!("Row {}: invalid or zero amount", index + 2));
                    continue;
                }
            };
            plan.rows.push(StatementRow {
                external_id: id,
                date,
                description: record[2].to_owned(),
                amount,
            });
        }
        if plan.rows.is_empty() && plan.issues.is_empty() {
            plan.issues.push("No statement rows found".into());
        }
        Ok(plan)
    }
    pub fn apply_bank_statement(&mut self, plan: &StatementPlan) -> Result<usize> {
        if !plan.can_apply() {
            return Err(Error::Validation(
                "Resolve statement preview issues first".into(),
            ));
        }
        let tx = self.conn.transaction()?;
        asset_ledger(&tx, plan.company, plan.ledger)?;
        let mut seen = HashSet::new();
        let mut stmt = tx.prepare("SELECT external_id FROM bank_statement_entries WHERE company_id = ?1 AND ledger_id = ?2")?;
        for row in stmt.query_map(params![plan.company, plan.ledger], |r| {
            r.get::<_, String>(0)
        })? {
            seen.insert(row?.trim().to_lowercase());
        }
        drop(stmt);
        for row in &plan.rows {
            if !seen.insert(row.external_id.to_lowercase()) {
                return Err(Error::Validation(
                    "A statement id already exists; preview the file again".into(),
                ));
            }
            tx.execute("INSERT INTO bank_statement_entries(company_id, ledger_id, external_id, date, description, amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![plan.company, plan.ledger, row.external_id, row.date, row.description, row.amount])?;
        }
        tx.commit()?;
        Ok(plan.rows.len())
    }
    pub fn bank_statement_entries(
        &self,
        company: i64,
        ledger: i64,
        as_of: &str,
    ) -> Result<Vec<StatementEntry>> {
        asset_ledger(&self.conn, company, ledger)?;
        let end = parse_date(as_of)?;
        let mut stmt = self.conn.prepare("SELECT s.id, s.external_id, s.date, s.description, s.amount, m.posting_id FROM bank_statement_entries s LEFT JOIN bank_matches m ON m.statement_id = s.id AND m.company_id = s.company_id WHERE s.company_id = ?1 AND s.ledger_id = ?2 ORDER BY s.date, s.id")?;
        let rows = stmt.query_map(params![company, ledger], |r| {
            Ok(StatementEntry {
                id: r.get(0)?,
                external_id: r.get(1)?,
                date: r.get(2)?,
                description: r.get(3)?,
                amount: r.get(4)?,
                matched_posting: r.get(5)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            let entry = row?;
            if parse_date(&entry.date)? <= end {
                result.push(entry);
            }
        }
        Ok(result)
    }
    pub fn match_bank_statement(
        &mut self,
        company: i64,
        ledger: i64,
        statement: i64,
        posting: i64,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason")?;
        let tx = self.conn.transaction()?;
        asset_ledger(&tx, company, ledger)?;
        let (date, amount): (String, i64) = tx.query_row("SELECT date, amount FROM bank_statement_entries WHERE id = ?1 AND company_id = ?2 AND ledger_id = ?3", params![statement, company, ledger], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Statement entry not found in this ledger".into()), other => Error::Database(other) })?;
        let (voucher, posting_amount): (i64, i64) = tx.query_row("SELECT v.id, p.amount FROM postings p JOIN vouchers v ON v.id = p.voucher_id WHERE p.id = ?1 AND p.ledger_id = ?2 AND v.company_id = ?3", params![posting, ledger, company], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Posting not found in this ledger".into()), other => Error::Database(other) })?;
        if posting_amount != amount {
            return Err(Error::Validation(
                "Statement and posting amounts must match exactly".into(),
            ));
        }
        let occupied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bank_matches WHERE statement_id = ?1 OR posting_id = ?2 UNION ALL SELECT 1 FROM bank_clearances WHERE posting_id = ?2)", params![statement, posting], |r| r.get(0))?;
        if occupied {
            return Err(Error::Validation(
                "Statement entry or posting is already matched or cleared".into(),
            ));
        }
        record_clearance(&tx, company, ledger, posting, Some(&date), reason)?;
        tx.execute(
            "INSERT INTO bank_matches(posting_id, statement_id, company_id) VALUES (?1, ?2, ?3)",
            params![posting, statement, company],
        )?;
        tx.execute("INSERT INTO bank_match_events(company_id, statement_id, voucher_id, posting_id, action, reason) VALUES (?1, ?2, ?3, ?4, 'match', ?5)", params![company, statement, voucher, posting, reason])?;
        tx.commit()?;
        Ok(())
    }
    pub fn unmatch_bank_statement(
        &mut self,
        company: i64,
        ledger: i64,
        statement: i64,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason")?;
        let tx = self.conn.transaction()?;
        asset_ledger(&tx, company, ledger)?;
        let (posting, voucher, date, clearance): (i64, i64, String, String) = tx.query_row("SELECT m.posting_id, v.id, s.date, c.cleared_on FROM bank_matches m JOIN bank_statement_entries s ON s.id = m.statement_id AND s.company_id = m.company_id JOIN postings p ON p.id = m.posting_id AND p.ledger_id = s.ledger_id JOIN vouchers v ON v.id = p.voucher_id AND v.company_id = s.company_id JOIN bank_clearances c ON c.posting_id = p.id AND c.company_id = s.company_id WHERE m.company_id = ?1 AND s.ledger_id = ?2 AND s.id = ?3", params![company, ledger, statement], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Matching entry not found or inconsistent".into()), other => Error::Database(other) })?;
        if date != clearance {
            return Err(Error::Validation(
                "Matching clearance is inconsistent".into(),
            ));
        }
        tx.execute(
            "DELETE FROM bank_matches WHERE company_id = ?1 AND statement_id = ?2",
            params![company, statement],
        )?;
        record_clearance(&tx, company, ledger, posting, None, reason)?;
        tx.execute("INSERT INTO bank_match_events(company_id, statement_id, voucher_id, posting_id, action, reason) VALUES (?1, ?2, ?3, ?4, 'unmatch', ?5)", params![company, statement, voucher, posting, reason])?;
        tx.commit()?;
        Ok(())
    }
    pub fn bank_match_events(&self, company: i64, statement: i64) -> Result<Vec<MatchEvent>> {
        let mut stmt = self.conn.prepare("SELECT changed_at, action, voucher_id, posting_id, reason FROM bank_match_events WHERE company_id = ?1 AND statement_id = ?2 ORDER BY id DESC")?;
        let rows = stmt
            .query_map(params![company, statement], |r| {
                Ok(MatchEvent {
                    changed_at: r.get(0)?,
                    action: r.get(1)?,
                    voucher_id: r.get(2)?,
                    posting_id: r.get(3)?,
                    reason: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Report book postings through an inclusive date, using current clearance dates.
    pub fn bank_report(&self, company: i64, ledger: i64, as_of: &str) -> Result<BankReport> {
        asset_ledger(&self.conn, company, ledger)?;
        let end = parse_date(as_of)?;
        let mut stmt = self.conn.prepare("SELECT p.id, v.id, v.date, v.reference, v.narration, p.amount, c.cleared_on FROM postings p JOIN vouchers v ON v.id = p.voucher_id LEFT JOIN bank_clearances c ON c.posting_id = p.id AND c.company_id = v.company_id WHERE v.company_id = ?1 AND p.ledger_id = ?2 ORDER BY v.date, v.id, p.id")?;
        let rows = stmt.query_map(params![company, ledger], |r| {
            Ok(BankEntry {
                posting_id: r.get(0)?,
                voucher_id: r.get(1)?,
                date: r.get(2)?,
                reference: r.get(3)?,
                narration: r.get(4)?,
                amount: r.get(5)?,
                cleared_on: r.get(6)?,
            })
        })?;
        let mut entries = Vec::new();
        let mut book = 0_i128;
        let mut cleared = 0_i128;
        for row in rows {
            let entry = row?;
            if parse_date(&entry.date)? > end {
                continue;
            }
            book += i128::from(entry.amount);
            if let Some(date) = &entry.cleared_on {
                if parse_date(date)? <= end {
                    cleared += i128::from(entry.amount);
                }
            }
            entries.push(entry);
        }
        Ok(BankReport {
            entries,
            book_balance: book,
            cleared_balance: cleared,
            uncleared: book - cleared,
        })
    }
    /// Set or remove a manual clearance, retaining a history of changes.
    pub fn set_bank_clearance(
        &mut self,
        company: i64,
        ledger: i64,
        posting: i64,
        date: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let matched: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM bank_matches WHERE posting_id = ?1 AND company_id = ?2)",
            params![posting, company],
            |r| r.get(0),
        )?;
        if matched {
            return Err(Error::Validation(
                "Unmatch the statement entry before changing its clearance".into(),
            ));
        }
        record_clearance(&tx, company, ledger, posting, date, reason)?;
        tx.commit()?;
        Ok(())
    }
    pub fn clearance_events(&self, company: i64, voucher: i64) -> Result<Vec<ClearanceEvent>> {
        let mut stmt = self.conn.prepare("SELECT e.changed_at, e.posting_id, l.name, e.amount, e.voucher_date, e.previous_date, e.new_date, e.reason FROM bank_clearance_events e JOIN ledgers l ON l.id = e.ledger_id AND l.company_id = e.company_id WHERE e.company_id = ?1 AND e.voucher_id = ?2 ORDER BY e.id DESC")?;
        let rows = stmt
            .query_map(params![company, voucher], |r| {
                Ok(ClearanceEvent {
                    changed_at: r.get(0)?,
                    posting_id: r.get(1)?,
                    ledger: r.get(2)?,
                    amount: r.get(3)?,
                    voucher_date: r.get(4)?,
                    previous_date: r.get(5)?,
                    new_date: r.get(6)?,
                    reason: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
