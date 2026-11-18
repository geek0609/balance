//! Versioned income and expense budgets with actuals from saved postings.
use crate::inventory::ledger_category;
use crate::{parse_date, required, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{HashMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE financial_budgets(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            name TEXT NOT NULL,
            name_key TEXT NOT NULL,
            UNIQUE(company_id, name_key)
        );
        CREATE TABLE budget_versions(
            id INTEGER PRIMARY KEY,
            budget_id INTEGER NOT NULL REFERENCES financial_budgets(id),
            start_date TEXT NOT NULL,
            end_date TEXT NOT NULL,
            centre_id INTEGER REFERENCES cost_centres(id),
            include_children INTEGER NOT NULL CHECK(include_children IN (0, 1)),
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            CHECK(centre_id IS NOT NULL OR include_children = 0)
        );
        CREATE INDEX budget_versions_budget ON budget_versions(budget_id, id);
        CREATE TABLE budget_targets(
            version_id INTEGER NOT NULL REFERENCES budget_versions(id),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            amount INTEGER NOT NULL CHECK(amount >= 0),
            PRIMARY KEY(version_id, ledger_id)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct BudgetTarget {
    pub ledger_id: i64,
    pub amount: i64,
}

#[derive(Debug, Clone)]
pub struct BudgetDraft {
    pub name: String,
    pub start_date: String,
    pub end_date: String,
    pub centre_id: Option<i64>,
    pub include_children: bool,
    pub reason: String,
    pub targets: Vec<BudgetTarget>,
}

#[derive(Debug, Clone)]
pub struct BudgetVersion {
    pub id: i64,
    pub name: String,
    pub start_date: String,
    pub end_date: String,
    pub centre_id: Option<i64>,
    pub centre: Option<String>,
    pub include_children: bool,
    pub reason: String,
    pub recorded_at: String,
    pub latest: bool,
}
impl BudgetVersion {
    pub fn scope_label(&self) -> String {
        match &self.centre {
            Some(name) if self.include_children => format!("{name} and subcentres"),
            Some(name) => name.clone(),
            None => "Company".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BudgetReportRow {
    pub ledger_id: i64,
    pub ledger: String,
    pub category: Category,
    pub planned: Option<i64>,
    pub actual: i128,
}
impl BudgetReportRow {
    pub fn favourable_variance(&self) -> i128 {
        let planned = i128::from(self.planned.unwrap_or(0));
        match self.category {
            Category::Income => self.actual - planned,
            Category::Expense => planned - self.actual,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BudgetTotals {
    pub income: i128,
    pub expenses: i128,
}
impl BudgetTotals {
    pub fn net(self) -> i128 {
        self.income - self.expenses
    }

    fn add(&mut self, category: Category, amount: i128) {
        if category == Category::Income {
            self.income += amount;
        } else {
            self.expenses += amount;
        }
    }
}

#[derive(Debug, Clone)]
pub struct BudgetReport {
    pub version: BudgetVersion,
    pub rows: Vec<BudgetReportRow>,
    pub planned: BudgetTotals,
    pub actual: BudgetTotals,
}

const VERSIONS: &str =
    "SELECT v.id, b.name, v.start_date, v.end_date, v.centre_id, c.name, v.include_children,
            v.reason, v.recorded_at,
            NOT EXISTS(SELECT 1 FROM budget_versions newer WHERE newer.budget_id = b.id AND newer.id > v.id)
     FROM budget_versions v JOIN financial_budgets b ON b.id = v.budget_id
     LEFT JOIN cost_centres c ON c.id = v.centre_id AND c.company_id = b.company_id
     WHERE b.company_id = ?1";

fn read_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<BudgetVersion> {
    Ok(BudgetVersion {
        id: row.get(0)?,
        name: row.get(1)?,
        start_date: row.get(2)?,
        end_date: row.get(3)?,
        centre_id: row.get(4)?,
        centre: row.get(5)?,
        include_children: row.get(6)?,
        reason: row.get(7)?,
        recorded_at: row.get(8)?,
        latest: row.get(9)?,
    })
}

fn checked_dates(from: &str, to: &str) -> Result<(NaiveDate, NaiveDate)> {
    let from = parse_date(from)?;
    let to = parse_date(to)?;
    if from > to {
        return Err(Error::Validation(
            "Budget start date cannot follow its end date".into(),
        ));
    }
    Ok((from, to))
}

fn actual_amounts(
    conn: &Connection,
    company: i64,
    scope: &BudgetVersion,
) -> Result<HashMap<i64, i128>> {
    let (from, to) = checked_dates(&scope.start_date, &scope.end_date)?;
    let whole_company = "SELECT p.ledger_id, v.date, p.amount
        FROM postings p JOIN vouchers v ON v.id = p.voucher_id
        JOIN ledgers l ON l.id = p.ledger_id AND l.company_id = v.company_id
        WHERE v.company_id = ?1 AND l.category IN ('Income','Expense')";
    let centre_only = "WITH RECURSIVE scoped(id) AS (
            SELECT ?2 AS id
            UNION
            SELECT c.id FROM cost_centres c JOIN scoped s ON s.id = c.parent_id
            WHERE c.company_id = ?1 AND ?3
        )
        SELECT p.ledger_id, v.date, CASE WHEN p.amount < 0 THEN -a.amount ELSE a.amount END
        FROM cost_allocations a JOIN scoped s ON s.id = a.centre_id
        JOIN postings p ON p.id = a.posting_id
        JOIN vouchers v ON v.id = p.voucher_id
        JOIN ledgers l ON l.id = p.ledger_id AND l.company_id = v.company_id
        WHERE v.company_id = ?1 AND l.category IN ('Income','Expense')";
    let mut stmt = conn.prepare(if scope.centre_id.is_some() {
        centre_only
    } else {
        whole_company
    })?;
    let mut rows = if let Some(centre) = scope.centre_id {
        if scope.centre.is_none() {
            return Err(Error::Validation(
                "Budget cost centre is missing or belongs to another company".into(),
            ));
        }
        stmt.query(params![company, centre, scope.include_children])?
    } else {
        stmt.query([company])?
    };
    let mut amounts = HashMap::<i64, i128>::new();
    while let Some(row) = rows.next()? {
        let date = parse_date(&row.get::<_, String>(1)?)?;
        if date >= from && date <= to {
            *amounts.entry(row.get(0)?).or_default() += i128::from(row.get::<_, i64>(2)?);
        }
    }
    Ok(amounts)
}

impl Store {
    /// Saving a name that already exists creates a new version.
    pub fn save_budget_version(&mut self, company: i64, draft: &BudgetDraft) -> Result<i64> {
        let name = required(&draft.name, "Budget name")?;
        let reason = required(&draft.reason, "Reason for saving the budget")?;
        let (from, to) = checked_dates(&draft.start_date, &draft.end_date)?;
        if draft.targets.is_empty() {
            return Err(Error::Validation("Enter at least one ledger target".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(centre) = draft.centre_id {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cost_centres WHERE id = ?1 AND company_id = ?2)",
                params![centre, company],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "Cost centre not found in this company".into(),
                ));
            }
        }
        let mut ledgers = HashSet::new();
        for target in &draft.targets {
            if target.amount < 0 || !ledgers.insert(target.ledger_id) {
                return Err(Error::Validation(
                    "Enter a non-negative amount once per ledger".into(),
                ));
            }
            if !matches!(
                ledger_category(&tx, company, target.ledger_id)?,
                Category::Income | Category::Expense
            ) {
                return Err(Error::Validation(
                    "Budgets require income or expense ledgers".into(),
                ));
            }
        }
        let key = name.to_lowercase();
        tx.execute(
            "INSERT INTO financial_budgets(company_id, name, name_key) VALUES (?1, ?2, ?3)
             ON CONFLICT(company_id, name_key) DO NOTHING",
            params![company, name, key],
        )?;
        let budget: i64 = tx.query_row(
            "SELECT id FROM financial_budgets WHERE company_id = ?1 AND name_key = ?2",
            params![company, key],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO budget_versions(budget_id, start_date, end_date, centre_id, include_children, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![budget, from.format("%Y-%m-%d").to_string(), to.format("%Y-%m-%d").to_string(),
                draft.centre_id, draft.centre_id.is_some() && draft.include_children, reason],
        )?;
        let version = tx.last_insert_rowid();
        for target in &draft.targets {
            tx.execute(
                "INSERT INTO budget_targets(version_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![version, target.ledger_id, target.amount],
            )?;
        }
        tx.commit()?;
        Ok(version)
    }

    pub fn budget_versions(&self, company: i64) -> Result<Vec<BudgetVersion>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{VERSIONS} ORDER BY b.name, v.id DESC"))?;
        let rows = stmt.query_map([company], read_version)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn budget_report(&self, company: i64, version: i64) -> Result<BudgetReport> {
        let tx = self.conn.unchecked_transaction()?;
        let version = tx
            .query_row(
                &format!("{VERSIONS} AND v.id = ?2"),
                params![company, version],
                read_version,
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Budget version not found in this company".into()))?;
        let mut actuals = actual_amounts(&tx, company, &version)?;
        let mut planned = BudgetTotals::default();
        let mut actual = BudgetTotals::default();
        let mut details = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT l.id, l.name, l.category, t.amount FROM ledgers l
                 LEFT JOIN budget_targets t ON t.ledger_id = l.id AND t.version_id = ?2
                 WHERE l.company_id = ?1 AND l.category IN ('Income','Expense')
                 ORDER BY CASE WHEN l.category = 'Income' THEN 0 ELSE 1 END, l.name, l.id",
            )?;
            let mut rows = stmt.query(params![company, version.id])?;
            while let Some(row) = rows.next()? {
                let ledger_id = row.get(0)?;
                let target: Option<i64> = row.get(3)?;
                let signed = actuals.remove(&ledger_id);
                if target.is_none() && signed.is_none() {
                    continue;
                }
                let category = Category::parse(&row.get::<_, String>(2)?)?;
                let value = if category == Category::Income {
                    -signed.unwrap_or(0)
                } else {
                    signed.unwrap_or(0)
                };
                planned.add(category, i128::from(target.unwrap_or(0)));
                actual.add(category, value);
                details.push(BudgetReportRow {
                    ledger_id,
                    ledger: row.get(1)?,
                    category,
                    planned: target,
                    actual: value,
                });
            }
        }
        tx.commit()?;
        Ok(BudgetReport {
            version,
            rows: details,
            planned,
            actual,
        })
    }
}
