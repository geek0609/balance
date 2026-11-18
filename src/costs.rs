//! Optional cost centre allocations for income and expense postings.
use crate::{parse_date, required, Category, Error, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE cost_centres(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            name TEXT NOT NULL,
            name_key TEXT NOT NULL,
            parent_id INTEGER REFERENCES cost_centres(id),
            UNIQUE(company_id, name_key)
        );
        CREATE TABLE cost_allocations(
            posting_id INTEGER NOT NULL REFERENCES postings(id),
            centre_id INTEGER NOT NULL REFERENCES cost_centres(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            PRIMARY KEY(posting_id, centre_id)
        );
        CREATE TABLE cost_allocation_changes(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            voucher_id INTEGER NOT NULL REFERENCES vouchers(id),
            posting_at_change INTEGER NOT NULL,
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            posting_amount INTEGER NOT NULL,
            reason TEXT NOT NULL,
            changed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX cost_changes_voucher ON cost_allocation_changes(company_id, voucher_id, id);
        CREATE TABLE cost_allocation_change_lines(
            change_id INTEGER NOT NULL REFERENCES cost_allocation_changes(id),
            centre_id INTEGER NOT NULL REFERENCES cost_centres(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            PRIMARY KEY(change_id, centre_id)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CostCentre {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostShare {
    pub centre_id: i64,
    pub amount: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostPosting {
    pub id: i64,
    pub voucher_id: i64,
    pub reference: String,
    pub date: String,
    pub ledger_id: i64,
    pub ledger: String,
    pub amount: i64,
}

#[derive(Debug, Clone)]
pub struct CostChange {
    pub changed_at: String,
    pub reason: String,
    pub posting_id: i64,
    pub ledger: String,
    pub posting_amount: i64,
    pub shares: Vec<CostShare>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CostTotals {
    pub income: i128,
    pub expenses: i128,
}
impl CostTotals {
    pub fn net(self) -> i128 {
        self.income - self.expenses
    }

    fn add_signed(&mut self, category: Category, signed: i128) {
        match category {
            Category::Income => self.income -= signed,
            Category::Expense => self.expenses += signed,
            _ => unreachable!("cost report only reads income and expense postings"),
        }
    }

    fn add(&mut self, other: Self) {
        self.income += other.income;
        self.expenses += other.expenses;
    }
}

#[derive(Debug, Clone)]
pub struct CostReportRow {
    pub centre: CostCentre,
    pub path: String,
    pub direct: CostTotals,
    pub including_children: CostTotals,
}
#[derive(Debug, Clone)]
pub struct CostReport {
    pub rows: Vec<CostReportRow>,
    pub unallocated: CostTotals,
    pub total: CostTotals,
}

const COST_POSTINGS: &str =
    "SELECT p.id, v.id, v.reference, v.date, l.id, l.name, p.amount
     FROM postings p JOIN vouchers v ON v.id = p.voucher_id
     JOIN ledgers l ON l.id = p.ledger_id AND l.company_id = v.company_id
     WHERE v.company_id = ?1 AND l.category IN ('Income', 'Expense')
        AND NOT EXISTS(SELECT 1 FROM opening_balances b WHERE b.voucher_id = v.id)
        AND NOT EXISTS(SELECT 1 FROM opening_balance_clears c WHERE v.id IN (c.original_voucher_id, c.clearing_voucher_id))";

fn read_posting(row: &rusqlite::Row<'_>) -> rusqlite::Result<CostPosting> {
    Ok(CostPosting {
        id: row.get(0)?,
        voucher_id: row.get(1)?,
        reference: row.get(2)?,
        date: row.get(3)?,
        ledger_id: row.get(4)?,
        ledger: row.get(5)?,
        amount: row.get(6)?,
    })
}

fn posting(conn: &Connection, company: i64, id: i64) -> Result<CostPosting> {
    let sql = format!("{COST_POSTINGS} AND p.id = ?2");
    conn.query_row(&sql, params![company, id], read_posting)
        .optional()?
        .ok_or_else(|| {
            Error::Validation("Income or expense posting not found in this company".into())
        })
}

fn allocation(conn: &Connection, posting: i64) -> Result<Vec<CostShare>> {
    let mut stmt = conn.prepare(
        "SELECT centre_id, amount FROM cost_allocations WHERE posting_id = ?1 ORDER BY centre_id",
    )?;
    let rows = stmt.query_map([posting], |row| {
        Ok(CostShare {
            centre_id: row.get(0)?,
            amount: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn write_allocation(
    conn: &Connection,
    company: i64,
    posting: &CostPosting,
    shares: &[CostShare],
    reason: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM cost_allocations WHERE posting_id = ?1",
        [posting.id],
    )?;
    conn.execute(
        "INSERT INTO cost_allocation_changes(company_id, voucher_id, posting_at_change, ledger_id, posting_amount, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![company, posting.voucher_id, posting.id, posting.ledger_id, posting.amount, reason],
    )?;
    let change = conn.last_insert_rowid();
    for share in shares {
        conn.execute(
            "INSERT INTO cost_allocations(posting_id, centre_id, amount) VALUES (?1, ?2, ?3)",
            params![posting.id, share.centre_id, share.amount],
        )?;
        conn.execute(
            "INSERT INTO cost_allocation_change_lines(change_id, centre_id, amount) VALUES (?1, ?2, ?3)",
            params![change, share.centre_id, share.amount],
        )?;
    }
    Ok(())
}

pub(crate) fn copy_reversal_allocation(
    conn: &Connection,
    company: i64,
    original: i64,
    reversal: i64,
) -> Result<()> {
    let shares = allocation(conn, original)?;
    if !shares.is_empty() {
        let target = posting(conn, company, reversal)?;
        write_allocation(
            conn,
            company,
            &target,
            &shares,
            "Carried forward from the reversed posting",
        )?;
    }
    Ok(())
}

pub(crate) fn ensure_unallocated(conn: &Connection, voucher: i64) -> Result<()> {
    let has_costs: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM postings p JOIN cost_allocations a ON a.posting_id = p.id
                        WHERE p.voucher_id = ?1)",
        [voucher],
        |row| row.get(0),
    )?;
    if has_costs {
        return Err(Error::Validation(
            "Clear this voucher's cost allocations before editing its postings".into(),
        ));
    }
    Ok(())
}

impl Store {
    pub fn create_cost_centre(
        &mut self,
        company: i64,
        name: &str,
        parent: Option<i64>,
    ) -> Result<i64> {
        let name = required(name, "Cost centre name")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(parent) = parent {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cost_centres WHERE id = ?1 AND company_id = ?2)",
                params![parent, company],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(Error::Validation(
                    "Parent cost centre not found in this company".into(),
                ));
            }
        }
        let wrote = tx.execute(
            "INSERT INTO cost_centres(company_id, name, name_key, parent_id) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(company_id, name_key) DO NOTHING",
            params![company, name, name.to_lowercase(), parent],
        )?;
        if wrote == 0 {
            return Err(Error::Validation("Cost centre name already exists".into()));
        }
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn cost_centres(&self, company: i64) -> Result<Vec<CostCentre>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id FROM cost_centres WHERE company_id = ?1 ORDER BY name, id",
        )?;
        let rows = stmt.query_map([company], |row| {
            Ok(CostCentre {
                id: row.get(0)?,
                name: row.get(1)?,
                parent_id: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Find the latest 100 matching income or expense postings.
    pub fn cost_postings(&self, company: i64, search: &str) -> Result<Vec<CostPosting>> {
        let mut stmt = self.conn.prepare(&format!(
            "{COST_POSTINGS}
             AND (v.id = ?3 OR (?3 IS NULL AND (?2 = '' OR instr(lower(v.reference), lower(?2)) > 0
                  OR instr(lower(l.name), lower(?2)) > 0)))
             ORDER BY v.date DESC, v.id DESC, p.id LIMIT 100"
        ))?;
        let search = search.trim();
        let voucher_id = search.parse::<i64>().ok();
        let rows = stmt.query_map(params![company, search, voucher_id], read_posting)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn posting_cost_shares(&self, company: i64, id: i64) -> Result<Vec<CostShare>> {
        posting(&self.conn, company, id)?;
        allocation(&self.conn, id)
    }

    pub fn save_cost_shares(
        &mut self,
        company: i64,
        expected: &CostPosting,
        previous: &[CostShare],
        shares: &[CostShare],
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason for changing the allocation")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = posting(&tx, company, expected.id)?;
        let saved = allocation(&tx, expected.id)?;
        if &current != expected || saved != previous {
            return Err(Error::Validation(
                "Posting or allocation changed. Reload it before saving.".into(),
            ));
        }
        let date = parse_date(&current.date)?;
        if crate::periods::cutoff(&tx, company)?.is_some_and(|cutoff| date <= cutoff) {
            return Err(Error::Validation(
                crate::periods::LOCKED_PERIOD_MESSAGE.into(),
            ));
        }
        let reversed: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM voucher_reversals r WHERE r.company_id = ?1 AND ?2 IN (r.original_voucher_id, r.reversal_voucher_id)
                UNION ALL
                SELECT 1 FROM invoice_reversals r JOIN invoices i ON i.id = r.invoice_id
                WHERE r.company_id = ?1 AND ?2 IN (i.voucher_id, r.reversal_voucher_id)
            )",
            params![company, current.voucher_id],
            |row| row.get(0),
        )?;
        if reversed {
            return Err(Error::Validation(
                "Allocations on a reversed voucher or its counter-entry are fixed".into(),
            ));
        }
        let mut used = HashSet::new();
        let mut total = 0_i128;
        for share in shares {
            if share.amount <= 0 || !used.insert(share.centre_id) {
                return Err(Error::Validation(
                    "Use a positive amount once per cost centre".into(),
                ));
            }
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cost_centres WHERE id = ?1 AND company_id = ?2)",
                params![share.centre_id, company],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(Error::Validation(
                    "Cost centre not found in this company".into(),
                ));
            }
            total += i128::from(share.amount);
        }
        if total > i128::from(current.amount).abs() {
            return Err(Error::Validation(
                "Allocations exceed the posting amount".into(),
            ));
        }
        let mut shares = shares.to_vec();
        shares.sort_by_key(|share| share.centre_id);
        if shares == saved {
            return Err(Error::Validation("The allocations have not changed".into()));
        }
        write_allocation(&tx, company, &current, &shares, reason)?;
        tx.commit()?;
        Ok(())
    }

    pub fn cost_history(&self, company: i64, voucher: i64) -> Result<Vec<CostChange>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.changed_at, c.reason, c.posting_at_change, l.name, c.posting_amount
             FROM cost_allocation_changes c JOIN ledgers l ON l.id = c.ledger_id
             WHERE c.company_id = ?1 AND c.voucher_id = ?2 ORDER BY c.id DESC",
        )?;
        let mut lines = self.conn.prepare(
            "SELECT centre_id, amount FROM cost_allocation_change_lines WHERE change_id = ?1 ORDER BY centre_id",
        )?;
        let mut rows = stmt.query(params![company, voucher])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let shares = lines
                .query_map([id], |row| {
                    Ok(CostShare {
                        centre_id: row.get(0)?,
                        amount: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            result.push(CostChange {
                changed_at: row.get(1)?,
                reason: row.get(2)?,
                posting_id: row.get(3)?,
                ledger: row.get(4)?,
                posting_amount: row.get(5)?,
                shares,
            });
        }
        Ok(result)
    }

    pub fn cost_report(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<CostReport> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(start), Some(end)) if start > end) {
            return Err(Error::Validation(
                "Report start date must not follow the end date".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let centres = self.cost_centres(company)?;
        let index: HashMap<_, _> = centres.iter().map(|centre| (centre.id, centre)).collect();
        let mut unallocated = CostTotals::default();
        let mut total = CostTotals::default();
        let mut direct = HashMap::<i64, CostTotals>::new();
        {
            let mut stmt = tx.prepare(
                "SELECT p.id, v.date, l.category, p.amount, a.centre_id, a.amount
                 FROM postings p JOIN vouchers v ON v.id = p.voucher_id
                 JOIN ledgers l ON l.id = p.ledger_id AND l.company_id = v.company_id
                 LEFT JOIN cost_allocations a ON a.posting_id = p.id
                 WHERE v.company_id = ?1 AND l.category IN ('Income', 'Expense')
                 ORDER BY p.id, a.centre_id",
            )?;
            let mut rows = stmt.query([company])?;
            let mut last = None;
            let mut amount_left = 0_i128;
            while let Some(row) = rows.next()? {
                let date = parse_date(&row.get::<_, String>(1)?)?;
                if start.is_some_and(|start| date < start) || end.is_some_and(|end| date > end) {
                    continue;
                }
                let id: i64 = row.get(0)?;
                let category = Category::parse(&row.get::<_, String>(2)?)?;
                let amount = i128::from(row.get::<_, i64>(3)?);
                if last != Some(id) {
                    last = Some(id);
                    amount_left = amount.abs();
                    unallocated.add_signed(category, amount);
                    total.add_signed(category, amount);
                }
                if let Some(centre_id) = row.get::<_, Option<i64>>(4)? {
                    let share = i128::from(row.get::<_, i64>(5)?);
                    if share <= 0 || share > amount_left || !index.contains_key(&centre_id) {
                        return Err(Error::Validation(
                            "Saved cost allocations are inconsistent".into(),
                        ));
                    }
                    amount_left -= share;
                    let signed = share * amount.signum();
                    direct
                        .entry(centre_id)
                        .or_default()
                        .add_signed(category, signed);
                    unallocated.add_signed(category, -signed);
                }
            }
        }
        let mut paths = BTreeMap::new();
        let mut inclusive = HashMap::<i64, CostTotals>::new();
        for centre in &centres {
            let amounts = direct.get(&centre.id).copied().unwrap_or_default();
            let mut chain = Vec::new();
            let mut visited = HashSet::new();
            let mut next = Some(centre.id);
            while let Some(id) = next {
                let current = index
                    .get(&id)
                    .ok_or_else(|| Error::Validation("Cost centre parent is missing".into()))?;
                if !visited.insert(id) {
                    return Err(Error::Validation(
                        "Cost centre hierarchy contains a cycle".into(),
                    ));
                }
                chain.push(current.name.as_str());
                inclusive.entry(id).or_default().add(amounts);
                next = current.parent_id;
            }
            chain.reverse();
            paths.insert(centre.id, chain.join(" / "));
        }
        let mut rows = centres
            .iter()
            .map(|centre| CostReportRow {
                centre: centre.clone(),
                path: paths.remove(&centre.id).unwrap_or_default(),
                direct: direct.get(&centre.id).copied().unwrap_or_default(),
                including_children: inclusive.get(&centre.id).copied().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.centre.id.cmp(&b.centre.id))
        });
        tx.commit()?;
        Ok(CostReport {
            rows,
            unallocated,
            total,
        })
    }
}
