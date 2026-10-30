//! Accounting primitives and an isolated SQLite store. Amounts are stored in
//! integer hundredths and parsed with decimal arithmetic, never floating point.

use chrono::NaiveDate;
use rusqlite::{params, Connection};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use std::{collections::HashMap, path::Path, str::FromStr};
use thiserror::Error;
pub mod exchange;
pub mod inventory;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Validation(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

/// A voucher classification. Tax and stock effects are not inferred from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoucherKind {
    Journal,
    Payment,
    Receipt,
    Transfer,
    Sales,
    Purchase,
    CreditNote,
    DebitNote,
}
impl VoucherKind {
    pub const ALL: [Self; 8] = [
        Self::Journal,
        Self::Payment,
        Self::Receipt,
        Self::Transfer,
        Self::Sales,
        Self::Purchase,
        Self::CreditNote,
        Self::DebitNote,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Journal => "Journal",
            Self::Payment => "Payment",
            Self::Receipt => "Receipt",
            Self::Transfer => "Transfer",
            Self::Sales => "Sales",
            Self::Purchase => "Purchase",
            Self::CreditNote => "Credit note",
            Self::DebitNote => "Debit note",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str() == value)
            .ok_or_else(|| Error::Validation("Unknown voucher type".into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Asset,
    Liability,
    Equity,
    Income,
    Expense,
}
impl Category {
    pub const ALL: [Self; 5] = [
        Self::Asset,
        Self::Liability,
        Self::Equity,
        Self::Income,
        Self::Expense,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asset => "Asset",
            Self::Liability => "Liability",
            Self::Equity => "Equity",
            Self::Income => "Income",
            Self::Expense => "Expense",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str() == value)
            .ok_or_else(|| Error::Validation("Unknown ledger category".into()))
    }
}

#[derive(Debug, Clone)]
pub struct Company {
    pub id: i64,
    pub name: String,
}
#[derive(Debug, Clone)]
pub struct Ledger {
    pub id: i64,
    pub name: String,
    pub category: Category,
    pub group_id: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct AccountGroup {
    pub id: i64,
    pub name: String,
    pub category: Category,
    pub parent_id: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct Posting {
    pub ledger_id: i64,
    /// Positive for debit, negative for credit, in hundredths.
    pub amount: i64,
}
#[derive(Debug, Clone)]
pub struct Voucher {
    pub id: i64,
    pub kind: VoucherKind,
    pub reference: String,
    pub date: String,
    pub narration: String,
    pub postings: Vec<Posting>,
}
#[derive(Debug, Clone)]
pub struct VoucherRevision {
    /// UTC timestamp at which this historical version was replaced.
    pub changed_at: String,
    pub voucher: Voucher,
}
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub voucher_id: i64,
    pub date: String,
    pub narration: String,
    pub amount: i64,
    /// Debit-minus-credit balance immediately after this posting.
    pub running_balance: i128,
}
#[derive(Debug, Clone)]
pub struct LedgerRegister {
    pub ledger: Ledger,
    pub opening_balance: i128,
    pub entries: Vec<LedgerEntry>,
    pub closing_balance: i128,
}
#[derive(Debug, Clone)]
pub struct Balance {
    pub ledger: Ledger,
    /// Debit minus credit, in hundredths.
    pub amount: i64,
}
/// Financial totals in hundredths. Profit is income less expenses.
#[derive(Debug, Clone)]
pub struct FinancialReport {
    pub trial_balance: Vec<Balance>,
    pub income: i128,
    pub expenses: i128,
    pub profit: i128,
    pub assets: i128,
    pub liabilities: i128,
    /// Includes cumulative net income through the report end date.
    pub equity: i128,
}

/// Parse a non-negative amount to hundredths, rejecting fractions of a hundredth.
pub fn parse_amount(input: &str) -> Result<i64> {
    let d = Decimal::from_str(input.trim())
        .map_err(|_| Error::Validation("Enter a valid decimal amount".into()))?;
    let scaled = d
        .checked_mul(Decimal::from(100))
        .ok_or_else(|| Error::Validation("Amount is too large".into()))?;
    if d < Decimal::ZERO || !scaled.fract().is_zero() {
        return Err(Error::Validation(
            "Amounts must be non-negative with at most two decimal places".into(),
        ));
    }
    scaled
        .to_i64()
        .ok_or_else(|| Error::Validation("Amount is too large".into()))
}
pub fn format_amount(value: i64) -> String {
    let n = value.unsigned_abs();
    format!(
        "{}{}.{:02}",
        if value < 0 { "-" } else { "" },
        n / 100,
        n % 100
    )
}

pub struct Store {
    conn: Connection,
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }
    pub fn memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    fn from_connection(mut conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 6 {
            return Err(Error::Validation(
                "This database was created by a newer version of the application".into(),
            ));
        }
        let tx = conn.transaction()?;
        if version == 0 {
            tx.execute_batch("
            CREATE TABLE IF NOT EXISTS companies(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
            CREATE TABLE IF NOT EXISTS ledgers(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), name TEXT NOT NULL, category TEXT NOT NULL, UNIQUE(company_id, name));
            CREATE TABLE IF NOT EXISTS vouchers(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), date TEXT NOT NULL, narration TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS postings(id INTEGER PRIMARY KEY, voucher_id INTEGER NOT NULL REFERENCES vouchers(id), ledger_id INTEGER NOT NULL REFERENCES ledgers(id), amount INTEGER NOT NULL CHECK(amount != 0));
            CREATE TABLE voucher_revisions(id INTEGER PRIMARY KEY, voucher_id INTEGER NOT NULL REFERENCES vouchers(id), company_id INTEGER NOT NULL REFERENCES companies(id), changed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, date TEXT NOT NULL, narration TEXT NOT NULL);
            CREATE TABLE revision_postings(id INTEGER PRIMARY KEY, revision_id INTEGER NOT NULL REFERENCES voucher_revisions(id), ledger_id INTEGER NOT NULL REFERENCES ledgers(id), amount INTEGER NOT NULL CHECK(amount != 0));
            CREATE INDEX vouchers_company_date ON vouchers(company_id, date);
            CREATE INDEX postings_voucher ON postings(voucher_id);
            CREATE INDEX revisions_voucher ON voucher_revisions(voucher_id, id);
            PRAGMA user_version = 1;")?;
        }
        if version < 2 {
            tx.execute_batch(
                "ALTER TABLE vouchers ADD COLUMN kind TEXT NOT NULL DEFAULT 'Journal';
                ALTER TABLE voucher_revisions ADD COLUMN kind TEXT NOT NULL DEFAULT 'Journal';
                PRAGMA user_version = 2;",
            )?;
        }
        if version < 3 {
            tx.execute_batch("CREATE TABLE account_groups(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), name TEXT NOT NULL, category TEXT NOT NULL, parent_id INTEGER REFERENCES account_groups(id), UNIQUE(company_id, name));
                ALTER TABLE ledgers ADD COLUMN group_id INTEGER REFERENCES account_groups(id);")?;
            let companies = {
                let mut stmt = tx.prepare("SELECT id FROM companies")?;
                let values = stmt
                    .query_map([], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                values
            };
            for company in companies {
                seed_groups(&tx, company)?;
                tx.execute("UPDATE ledgers SET group_id = (SELECT id FROM account_groups WHERE company_id = ?1 AND name = ledgers.category) WHERE company_id = ?1", [company])?;
            }
            tx.execute_batch("PRAGMA user_version = 3;")?;
        }
        if version < 4 {
            tx.execute_batch(
                "ALTER TABLE vouchers ADD COLUMN reference TEXT NOT NULL DEFAULT '';
                ALTER TABLE voucher_revisions ADD COLUMN reference TEXT NOT NULL DEFAULT '';
                PRAGMA user_version = 4;",
            )?;
        }
        if version < 5 {
            tx.execute_batch("CREATE TABLE stock_items(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), name TEXT NOT NULL, unit TEXT NOT NULL, UNIQUE(company_id, name));
                CREATE TABLE stock_adjustments(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), item_id INTEGER NOT NULL REFERENCES stock_items(id), date TEXT NOT NULL, quantity INTEGER NOT NULL CHECK(quantity != 0), reason TEXT NOT NULL);
                CREATE TABLE invoices(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id), kind TEXT NOT NULL CHECK(kind IN ('Sale', 'Purchase')), party_ledger_id INTEGER NOT NULL REFERENCES ledgers(id), trade_ledger_id INTEGER NOT NULL REFERENCES ledgers(id), tax_ledger_id INTEGER REFERENCES ledgers(id), tax_amount INTEGER NOT NULL CHECK(tax_amount >= 0));
                CREATE TABLE invoice_lines(id INTEGER PRIMARY KEY, invoice_id INTEGER NOT NULL REFERENCES invoices(id), item_id INTEGER NOT NULL REFERENCES stock_items(id), quantity INTEGER NOT NULL CHECK(quantity > 0), unit_price INTEGER NOT NULL CHECK(unit_price >= 0), amount INTEGER NOT NULL CHECK(amount >= 0));
                CREATE INDEX stock_items_company ON stock_items(company_id, name);
                CREATE INDEX stock_adjustments_item ON stock_adjustments(item_id, date);
                CREATE INDEX invoices_company ON invoices(company_id, id);
                CREATE INDEX invoice_lines_invoice ON invoice_lines(invoice_id);
                PRAGMA user_version = 5;")?;
        }
        if version < 6 {
            tx.execute_batch("CREATE TABLE invoice_settlements(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), invoice_id INTEGER NOT NULL REFERENCES invoices(id), voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id), amount INTEGER NOT NULL CHECK(amount > 0));
                CREATE INDEX settlements_invoice ON invoice_settlements(invoice_id, id);
                PRAGMA user_version = 6;")?;
        }
        tx.commit()?;
        Ok(Self { conn })
    }
    pub fn create_company(&self, name: &str) -> Result<i64> {
        let name = required(name, "Company name")?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("INSERT INTO companies(name) VALUES (?1)", [name])?;
        let id = tx.last_insert_rowid();
        seed_groups(&tx, id)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn companies(&self) -> Result<Vec<Company>> {
        let mut s = self
            .conn
            .prepare("SELECT id, name FROM companies ORDER BY name")?;
        let rows = s
            .query_map([], |r| {
                Ok(Company {
                    id: r.get(0)?,
                    name: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn create_ledger(&self, company_id: i64, name: &str, category: Category) -> Result<i64> {
        let group_id: i64 = self.conn.query_row("SELECT id FROM account_groups WHERE company_id = ?1 AND name = ?2 AND parent_id IS NULL", params![company_id, category.as_str()], |r| r.get(0))?;
        self.create_ledger_in_group(company_id, name, group_id)
    }
    pub fn create_ledger_in_group(
        &self,
        company_id: i64,
        name: &str,
        group_id: i64,
    ) -> Result<i64> {
        let name = required(name, "Ledger name")?;
        let category: String = self
            .conn
            .query_row(
                "SELECT category FROM account_groups WHERE id = ?1 AND company_id = ?2",
                params![group_id, company_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Group not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        self.conn.execute(
            "INSERT INTO ledgers(company_id, name, category, group_id) VALUES (?1, ?2, ?3, ?4)",
            params![company_id, name, category, group_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn ledgers(&self, company_id: i64) -> Result<Vec<Ledger>> {
        let mut s = self.conn.prepare(
            "SELECT id, name, category, group_id FROM ledgers WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = s
            .query_map([company_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<(i64, String, String, Option<i64>)>>>()?;
        rows.into_iter()
            .map(|(id, name, category, group_id)| {
                Ok(Ledger {
                    id,
                    name,
                    category: Category::parse(&category)?,
                    group_id,
                })
            })
            .collect()
    }
    pub fn account_groups(&self, company_id: i64) -> Result<Vec<AccountGroup>> {
        let mut stmt = self.conn.prepare("SELECT id, name, category, parent_id FROM account_groups WHERE company_id = ?1 ORDER BY name")?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<(i64, String, String, Option<i64>)>>>()?;
        rows.into_iter()
            .map(|(id, name, category, parent_id)| {
                Ok(AccountGroup {
                    id,
                    name,
                    parent_id,
                    category: Category::parse(&category)?,
                })
            })
            .collect()
    }
    pub fn create_group(
        &self,
        company_id: i64,
        name: &str,
        parent_id: Option<i64>,
        root_category: Category,
    ) -> Result<i64> {
        let name = required(name, "Group name")?;
        let category = if let Some(parent) = parent_id {
            self.conn
                .query_row(
                    "SELECT category FROM account_groups WHERE id = ?1 AND company_id = ?2",
                    params![parent, company_id],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => {
                        Error::Validation("Parent group not found in the selected company".into())
                    }
                    other => Error::Database(other),
                })?
        } else {
            root_category.as_str().to_owned()
        };
        self.conn.execute("INSERT INTO account_groups(company_id, name, category, parent_id) VALUES (?1, ?2, ?3, ?4)", params![company_id, name, category, parent_id])?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn post_voucher(
        &mut self,
        company_id: i64,
        date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<i64> {
        self.post_typed_voucher(company_id, VoucherKind::Journal, date, narration, postings)
    }
    pub fn post_typed_voucher(
        &mut self,
        company_id: i64,
        kind: VoucherKind,
        date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<i64> {
        self.post_voucher_with_reference(company_id, kind, "", date, narration, postings)
    }
    pub fn post_voucher_with_reference(
        &mut self,
        company_id: i64,
        kind: VoucherKind,
        reference: &str,
        date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<i64> {
        let date = validate_voucher(date, postings)?;
        let tx = self.conn.transaction()?;
        check_ledgers(&tx, company_id, postings)?;
        tx.execute(
            "INSERT INTO vouchers(company_id, date, narration, kind, reference) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                company_id,
                date.format("%Y-%m-%d").to_string(),
                narration.trim(),
                kind.as_str(),
                reference.trim()
            ],
        )?;
        let id = tx.last_insert_rowid();
        for p in postings {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![id, p.ledger_id, p.amount],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }
    /// Replace a voucher and its postings in one transaction.
    pub fn update_voucher(
        &mut self,
        company_id: i64,
        voucher_id: i64,
        date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<()> {
        let kind: String = self
            .conn
            .query_row(
                "SELECT kind FROM vouchers WHERE id = ?1 AND company_id = ?2",
                params![voucher_id, company_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Voucher not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        self.update_typed_voucher(
            company_id,
            voucher_id,
            VoucherKind::parse(&kind)?,
            date,
            narration,
            postings,
        )
    }
    pub fn update_typed_voucher(
        &mut self,
        company_id: i64,
        voucher_id: i64,
        kind: VoucherKind,
        date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<()> {
        let reference: String = self
            .conn
            .query_row(
                "SELECT reference FROM vouchers WHERE id = ?1 AND company_id = ?2",
                params![voucher_id, company_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Voucher not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        self.update_voucher_with_reference(
            company_id, voucher_id, kind, &reference, date, narration, postings,
        )
    }
    pub fn update_voucher_with_reference(
        &mut self,
        company_id: i64,
        voucher_id: i64,
        kind: VoucherKind,
        reference: &str,
        date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<()> {
        let date = validate_voucher(date, postings)?;
        let tx = self.conn.transaction()?;
        check_ledgers(&tx, company_id, postings)?;
        let invoice_backed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM invoices WHERE voucher_id = ?1 AND company_id = ?2 UNION ALL SELECT 1 FROM invoice_settlements WHERE voucher_id = ?1 AND company_id = ?2)",
            params![voucher_id, company_id],
            |r| r.get(0),
        )?;
        if invoice_backed {
            return Err(Error::Validation(
                "Invoice and settlement postings cannot be edited from the voucher screen".into(),
            ));
        }
        let saved = tx.execute("INSERT INTO voucher_revisions(voucher_id, company_id, date, narration, kind, reference) SELECT id, company_id, date, narration, kind, reference FROM vouchers WHERE id = ?1 AND company_id = ?2", params![voucher_id, company_id])?;
        if saved != 1 {
            return Err(Error::Validation(
                "Voucher not found in the selected company".into(),
            ));
        }
        let revision_id = tx.last_insert_rowid();
        tx.execute("INSERT INTO revision_postings(revision_id, ledger_id, amount) SELECT ?1, ledger_id, amount FROM postings WHERE voucher_id = ?2 ORDER BY id", params![revision_id, voucher_id])?;
        let changed = tx.execute(
            "UPDATE vouchers SET date = ?1, narration = ?2, kind = ?5, reference = ?6 WHERE id = ?3 AND company_id = ?4",
            params![
                date.format("%Y-%m-%d").to_string(),
                narration.trim(),
                voucher_id,
                company_id,
                kind.as_str(),
                reference.trim()
            ],
        )?;
        if changed != 1 {
            return Err(Error::Validation(
                "Voucher not found in the selected company".into(),
            ));
        }
        tx.execute("DELETE FROM postings WHERE voucher_id = ?1", [voucher_id])?;
        for p in postings {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![voucher_id, p.ledger_id, p.amount],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Read only vouchers belonging to the given company, newest first.
    pub fn vouchers(&self, company_id: i64) -> Result<Vec<Voucher>> {
        let mut stmt = self.conn.prepare("SELECT id, date, narration, kind, reference FROM vouchers WHERE company_id = ?1 ORDER BY date DESC, id DESC")?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<(i64, String, String, String, String)>>>()?;
        let mut postings_stmt = self
            .conn
            .prepare("SELECT ledger_id, amount FROM postings WHERE voucher_id = ?1 ORDER BY id")?;
        let mut vouchers = Vec::with_capacity(rows.len());
        for (id, date, narration, kind, reference) in rows {
            let postings = postings_stmt
                .query_map([id], |r| {
                    Ok(Posting {
                        ledger_id: r.get(0)?,
                        amount: r.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            vouchers.push(Voucher {
                id,
                kind: VoucherKind::parse(&kind)?,
                reference,
                date,
                narration,
                postings,
            });
        }
        // Older databases may contain valid dates without zero-padding.
        for voucher in &mut vouchers {
            voucher.date = parse_date(&voucher.date)?.format("%Y-%m-%d").to_string();
        }
        vouchers.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
        Ok(vouchers)
    }
    pub fn voucher_revisions(
        &self,
        company_id: i64,
        voucher_id: i64,
    ) -> Result<Vec<VoucherRevision>> {
        let mut stmt = self.conn.prepare("SELECT id, changed_at, date, narration, kind, reference FROM voucher_revisions WHERE company_id = ?1 AND voucher_id = ?2 ORDER BY id DESC")?;
        let rows = stmt
            .query_map(params![company_id, voucher_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<(i64, String, String, String, String, String)>>>()?;
        let mut posting_stmt = self.conn.prepare(
            "SELECT ledger_id, amount FROM revision_postings WHERE revision_id = ?1 ORDER BY id",
        )?;
        let mut revisions = Vec::with_capacity(rows.len());
        for (revision_id, changed_at, date, narration, kind, reference) in rows {
            let postings = posting_stmt
                .query_map([revision_id], |r| {
                    Ok(Posting {
                        ledger_id: r.get(0)?,
                        amount: r.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            revisions.push(VoucherRevision {
                changed_at,
                voucher: Voucher {
                    id: voucher_id,
                    kind: VoucherKind::parse(&kind)?,
                    reference,
                    date,
                    narration,
                    postings,
                },
            });
        }
        Ok(revisions)
    }
    pub fn balances(&self, company_id: i64) -> Result<Vec<Balance>> {
        self.balances_in_period(company_id, None, None)
    }
    /// Entries in date and voucher order, including a calculated opening balance.
    pub fn ledger_register(
        &self,
        company_id: i64,
        ledger_id: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<LedgerRegister> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(a), Some(b)) if a > b) {
            return Err(Error::Validation(
                "The start date must not be after the end date".into(),
            ));
        }
        let ledger = self
            .ledgers(company_id)?
            .into_iter()
            .find(|l| l.id == ledger_id)
            .ok_or_else(|| Error::Validation("Ledger not found in the selected company".into()))?;
        let mut opening_balance = 0_i128;
        let mut running = 0_i128;
        let mut entries = Vec::new();
        for voucher in self.vouchers(company_id)?.into_iter().rev() {
            let date = parse_date(&voucher.date)?;
            if end.is_some_and(|v| date > v) {
                continue;
            }
            for posting in voucher.postings.iter().filter(|p| p.ledger_id == ledger_id) {
                running += i128::from(posting.amount);
                if start.is_some_and(|v| date < v) {
                    opening_balance = running;
                } else {
                    entries.push(LedgerEntry {
                        voucher_id: voucher.id,
                        date: voucher.date.clone(),
                        narration: voucher.narration.clone(),
                        amount: posting.amount,
                        running_balance: running,
                    });
                }
            }
        }
        Ok(LedgerRegister {
            ledger,
            opening_balance,
            entries,
            closing_balance: running,
        })
    }
    /// Return debit-minus-credit balances for an inclusive date range.
    pub fn balances_in_period(
        &self,
        company_id: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Vec<Balance>> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(a), Some(b)) if a > b) {
            return Err(Error::Validation(
                "The start date must not be after the end date".into(),
            ));
        }
        let ledgers = self.ledgers(company_id)?;
        let mut sums = HashMap::<i64, i128>::new();
        let mut s = self.conn.prepare("SELECT p.ledger_id, p.amount, v.date FROM postings p JOIN vouchers v ON p.voucher_id = v.id WHERE v.company_id = ?1")?;
        let rows = s.query_map([company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, amount, date) = row?;
            let date = parse_date(&date)?;
            if start.is_some_and(|v| date < v) || end.is_some_and(|v| date > v) {
                continue;
            }
            *sums.entry(id).or_default() += i128::from(amount);
        }
        ledgers
            .into_iter()
            .map(|ledger| {
                let amount = i64::try_from(*sums.get(&ledger.id).unwrap_or(&0))
                    .map_err(|_| Error::Validation("Balance exceeds supported range".into()))?;
                Ok(Balance { amount, ledger })
            })
            .collect()
    }
    /// Trial balance and balance sheet are as of `end`; profit and loss is
    /// inclusive from `start` through `end`. None denotes an open boundary.
    pub fn financial_report(
        &self,
        company_id: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<FinancialReport> {
        let period = self.balances_in_period(company_id, start, end)?;
        let trial_balance = self.balances_in_period(company_id, None, end)?;
        let category_total = |balances: &[Balance], category: Category| -> i128 {
            balances
                .iter()
                .filter(|b| b.ledger.category == category)
                .map(|b| i128::from(b.amount))
                .sum()
        };
        let income = -category_total(&period, Category::Income);
        let expenses = category_total(&period, Category::Expense);
        let cumulative_profit = -category_total(&trial_balance, Category::Income)
            - category_total(&trial_balance, Category::Expense);
        Ok(FinancialReport {
            assets: category_total(&trial_balance, Category::Asset),
            liabilities: -category_total(&trial_balance, Category::Liability),
            equity: -category_total(&trial_balance, Category::Equity) + cumulative_profit,
            profit: income - expenses,
            income,
            expenses,
            trial_balance,
        })
    }
}
fn seed_groups(conn: &Connection, company_id: i64) -> Result<()> {
    let groups = [
        ("Asset", Category::Asset, None),
        ("Liability", Category::Liability, None),
        ("Equity", Category::Equity, None),
        ("Income", Category::Income, None),
        ("Expense", Category::Expense, None),
        ("Current Assets", Category::Asset, Some("Asset")),
        ("Fixed Assets", Category::Asset, Some("Asset")),
        ("Investments", Category::Asset, Some("Asset")),
        ("Bank Accounts", Category::Asset, Some("Current Assets")),
        ("Cash-in-Hand", Category::Asset, Some("Current Assets")),
        ("Sundry Debtors", Category::Asset, Some("Current Assets")),
        ("Stock-in-Hand", Category::Asset, Some("Current Assets")),
        ("Deposits (Asset)", Category::Asset, Some("Current Assets")),
        (
            "Loans & Advances (Asset)",
            Category::Asset,
            Some("Current Assets"),
        ),
        (
            "Current Liabilities",
            Category::Liability,
            Some("Liability"),
        ),
        (
            "Sundry Creditors",
            Category::Liability,
            Some("Current Liabilities"),
        ),
        (
            "Duties & Taxes",
            Category::Liability,
            Some("Current Liabilities"),
        ),
        (
            "Provisions",
            Category::Liability,
            Some("Current Liabilities"),
        ),
        ("Loans (Liability)", Category::Liability, Some("Liability")),
        (
            "Secured Loans",
            Category::Liability,
            Some("Loans (Liability)"),
        ),
        (
            "Unsecured Loans",
            Category::Liability,
            Some("Loans (Liability)"),
        ),
        ("Capital Account", Category::Equity, Some("Equity")),
        ("Reserves & Surplus", Category::Equity, Some("Equity")),
        ("Sales Accounts", Category::Income, Some("Income")),
        ("Direct Incomes", Category::Income, Some("Income")),
        ("Indirect Incomes", Category::Income, Some("Income")),
        ("Purchase Accounts", Category::Expense, Some("Expense")),
        ("Direct Expenses", Category::Expense, Some("Expense")),
        ("Indirect Expenses", Category::Expense, Some("Expense")),
    ];
    let mut ids = HashMap::new();
    for (name, category, parent) in groups {
        let parent_id = parent.map(|p| ids[p]);
        conn.execute("INSERT INTO account_groups(company_id, name, category, parent_id) VALUES (?1, ?2, ?3, ?4)", params![company_id, name, category.as_str(), parent_id])?;
        ids.insert(name, conn.last_insert_rowid());
    }
    Ok(())
}
fn parse_date(date: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map_err(|_| Error::Validation("Date must be a valid YYYY-MM-DD date".into()))
}
fn validate_voucher(date: &str, postings: &[Posting]) -> Result<NaiveDate> {
    let date = parse_date(date)?;
    if postings.len() < 2 || postings.iter().any(|p| p.amount == 0) {
        return Err(Error::Validation(
            "A voucher requires at least two non-zero postings".into(),
        ));
    }
    if !postings
        .iter()
        .any(|p| p.ledger_id != postings[0].ledger_id)
    {
        return Err(Error::Validation(
            "A voucher requires at least two different ledgers".into(),
        ));
    }
    if postings.iter().map(|p| i128::from(p.amount)).sum::<i128>() != 0 {
        return Err(Error::Validation("Debits and credits must balance".into()));
    }
    Ok(date)
}
fn check_ledgers(conn: &Connection, company_id: i64, postings: &[Posting]) -> Result<()> {
    for p in postings {
        let found: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ledgers WHERE id = ?1 AND company_id = ?2)",
            params![p.ledger_id, company_id],
            |r| r.get(0),
        )?;
        if !found {
            return Err(Error::Validation(
                "Every ledger must belong to the selected company".into(),
            ));
        }
    }
    Ok(())
}
fn required<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        Err(Error::Validation(format!("{label} is required")))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_groups_inherit_categories_and_are_company_scoped() {
        let db = Store::memory().unwrap();
        let a = db.create_company("A").unwrap();
        let b = db.create_company("B").unwrap();
        let groups = db.account_groups(a).unwrap();
        let receivables = groups.iter().find(|g| g.name == "Sundry Debtors").unwrap();
        let regional = db
            .create_group(
                a,
                "Regional accounts",
                Some(receivables.id),
                Category::Expense,
            )
            .unwrap();
        let ledger_id = db.create_ledger_in_group(a, "Customer", regional).unwrap();
        let customer = db
            .ledgers(a)
            .unwrap()
            .into_iter()
            .find(|l| l.id == ledger_id)
            .unwrap();
        assert_eq!(customer.category, Category::Asset);
        assert_eq!(customer.group_id, Some(regional));
        assert!(db
            .create_group(b, "Wrong", Some(receivables.id), Category::Asset)
            .is_err());
        assert!(db.create_ledger_in_group(b, "Wrong", regional).is_err());
    }
    #[test]
    fn voucher_kind_survives_edits_and_is_recorded_in_revisions() {
        let mut db = Store::memory().unwrap();
        let company = db.create_company("Example").unwrap();
        let cash = db.create_ledger(company, "Cash", Category::Asset).unwrap();
        let income = db
            .create_ledger(company, "Income", Category::Income)
            .unwrap();
        let postings = [
            Posting {
                ledger_id: cash,
                amount: 100,
            },
            Posting {
                ledger_id: income,
                amount: -100,
            },
        ];
        let id = db
            .post_typed_voucher(
                company,
                VoucherKind::Sales,
                "2026-01-01",
                "first",
                &postings,
            )
            .unwrap();
        db.update_voucher(company, id, "2026-01-02", "second", &postings)
            .unwrap();
        assert_eq!(db.vouchers(company).unwrap()[0].kind, VoucherKind::Sales);
        db.update_typed_voucher(
            company,
            id,
            VoucherKind::Receipt,
            "2026-01-02",
            "third",
            &postings,
        )
        .unwrap();
        assert_eq!(db.vouchers(company).unwrap()[0].kind, VoucherKind::Receipt);
        assert_eq!(
            db.voucher_revisions(company, id).unwrap()[0].voucher.kind,
            VoucherKind::Sales
        );
    }
    #[test]
    fn ledger_register_tracks_opening_running_and_closing_balances() {
        let mut db = Store::memory().unwrap();
        let company = db.create_company("Example").unwrap();
        let other = db.create_company("Other").unwrap();
        let cash = db.create_ledger(company, "Cash", Category::Asset).unwrap();
        let equity = db
            .create_ledger(company, "Capital", Category::Equity)
            .unwrap();
        for (date, amount) in [
            ("2026-03-01", 50),
            ("2026-01-01", 100),
            ("2026-02-01", -30),
            ("2026-04-01", 25),
        ] {
            db.post_voucher(
                company,
                date,
                date,
                &[
                    Posting {
                        ledger_id: cash,
                        amount,
                    },
                    Posting {
                        ledger_id: equity,
                        amount: -amount,
                    },
                ],
            )
            .unwrap();
        }
        let register = db
            .ledger_register(company, cash, Some("2026-02-01"), Some("2026-03-31"))
            .unwrap();
        assert_eq!(register.opening_balance, 100);
        assert_eq!(register.entries.len(), 2);
        assert_eq!(
            (
                register.entries[0].date.as_str(),
                register.entries[0].running_balance
            ),
            ("2026-02-01", 70)
        );
        assert_eq!(
            (
                register.entries[1].date.as_str(),
                register.entries[1].running_balance
            ),
            ("2026-03-01", 120)
        );
        assert_eq!(register.closing_balance, 120);
        assert!(db.ledger_register(other, cash, None, None).is_err());
        assert!(db
            .ledger_register(company, cash, Some("2026-03-01"), Some("2026-02-01"))
            .is_err());
    }
    #[test]
    fn upgrades_existing_database_and_retains_prior_voucher_versions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE companies(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
            CREATE TABLE ledgers(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), name TEXT NOT NULL, category TEXT NOT NULL, UNIQUE(company_id, name));
            CREATE TABLE vouchers(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), date TEXT NOT NULL, narration TEXT NOT NULL);
            CREATE TABLE postings(id INTEGER PRIMARY KEY, voucher_id INTEGER NOT NULL REFERENCES vouchers(id), ledger_id INTEGER NOT NULL REFERENCES ledgers(id), amount INTEGER NOT NULL CHECK(amount != 0));
            INSERT INTO companies VALUES (1, 'Example');
            INSERT INTO ledgers VALUES (1, 1, 'Cash', 'Asset'), (2, 1, 'Capital', 'Equity');
            INSERT INTO vouchers VALUES (1, 1, '2026-01-01', 'Original');
            INSERT INTO postings VALUES (1, 1, 1, 100), (2, 1, 2, -100);").unwrap();
        let mut db = Store::from_connection(conn).unwrap();
        assert_eq!(db.vouchers(1).unwrap()[0].narration, "Original");
        db.update_voucher(
            1,
            1,
            "2026-01-02",
            "Corrected",
            &[
                Posting {
                    ledger_id: 1,
                    amount: 200,
                },
                Posting {
                    ledger_id: 2,
                    amount: -200,
                },
            ],
        )
        .unwrap();
        let revisions = db.voucher_revisions(1, 1).unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].voucher.narration, "Original");
        assert_eq!(revisions[0].voucher.postings[0].amount, 100);
        assert!(db.voucher_revisions(2, 1).unwrap().is_empty());
        assert!(db
            .update_voucher(
                1,
                1,
                "2026-01-03",
                "invalid",
                &[
                    Posting {
                        ledger_id: 1,
                        amount: 2
                    },
                    Posting {
                        ledger_id: 2,
                        amount: -1
                    }
                ]
            )
            .is_err());
        assert_eq!(db.voucher_revisions(1, 1).unwrap().len(), 1);
        assert_eq!(db.vouchers(1).unwrap()[0].narration, "Corrected");
        let version: i64 = db
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 6);
    }
    #[test]
    fn rejects_newer_database_versions_without_modifying_them() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        assert!(matches!(
            Store::from_connection(conn),
            Err(Error::Validation(_))
        ));
    }
    #[test]
    fn reports_apply_period_to_profit_and_end_date_to_balance_sheet() {
        let mut db = Store::memory().unwrap();
        let company = db.create_company("Example").unwrap();
        let cash = db.create_ledger(company, "Cash", Category::Asset).unwrap();
        let capital = db
            .create_ledger(company, "Capital", Category::Equity)
            .unwrap();
        let income = db
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let expenses = db
            .create_ledger(company, "Rent", Category::Expense)
            .unwrap();
        let post = |db: &mut Store, date: &str, debit: i64, credit: i64, amount: i64| {
            db.post_voucher(
                company,
                date,
                "",
                &[
                    Posting {
                        ledger_id: debit,
                        amount,
                    },
                    Posting {
                        ledger_id: credit,
                        amount: -amount,
                    },
                ],
            )
            .unwrap();
        };
        post(&mut db, "2025-01-01", cash, capital, 10_000);
        post(&mut db, "2025-06-01", cash, income, 2_000);
        post(&mut db, "2026-01-01", expenses, cash, 500);
        post(&mut db, "2027-01-01", cash, income, 4_000);
        let report = db
            .financial_report(company, Some("2026-01-01"), Some("2026-12-31"))
            .unwrap();
        assert_eq!(
            (report.income, report.expenses, report.profit),
            (0, 500, -500)
        );
        assert_eq!(
            (report.assets, report.liabilities, report.equity),
            (11_500, 0, 11_500)
        );
        assert_eq!(
            report
                .trial_balance
                .iter()
                .map(|b| i128::from(b.amount))
                .sum::<i128>(),
            0
        );
        let early = db
            .financial_report(company, None, Some("2025-12-31"))
            .unwrap();
        assert_eq!(
            (early.income, early.profit, early.assets, early.equity),
            (2_000, 2_000, 12_000, 12_000)
        );
        assert!(db
            .financial_report(company, Some("2027-01-01"), Some("2026-12-31"))
            .is_err());
        assert!(db
            .financial_report(company, Some("2026-02-30"), None)
            .is_err());
    }
    #[test]
    fn voucher_history_and_edits_are_scoped_and_atomic() {
        let mut db = Store::memory().unwrap();
        let a = db.create_company("A").unwrap();
        let b = db.create_company("B").unwrap();
        let cash = db.create_ledger(a, "Cash", Category::Asset).unwrap();
        let income = db.create_ledger(a, "Income", Category::Income).unwrap();
        let other = db.create_ledger(b, "Other", Category::Asset).unwrap();
        let original = [
            Posting {
                ledger_id: cash,
                amount: 500,
            },
            Posting {
                ledger_id: income,
                amount: -500,
            },
        ];
        let id = db.post_voucher(a, "2026-1-2", "first", &original).unwrap();
        assert_eq!(db.vouchers(a).unwrap()[0].date, "2026-01-02");
        assert!(db.vouchers(b).unwrap().is_empty());
        assert!(db
            .update_voucher(b, id, "2026-01-03", "wrong company", &original)
            .is_err());
        assert!(db
            .update_voucher(
                a,
                id,
                "2026-01-03",
                "bad",
                &[
                    Posting {
                        ledger_id: cash,
                        amount: 700
                    },
                    Posting {
                        ledger_id: other,
                        amount: -700
                    }
                ]
            )
            .is_err());
        let saved = db.vouchers(a).unwrap();
        assert_eq!(saved[0].narration, "first");
        assert_eq!(saved[0].postings[0].amount, 500);
        db.update_voucher(
            a,
            id,
            "2026-01-03",
            "corrected",
            &[
                Posting {
                    ledger_id: cash,
                    amount: 700,
                },
                Posting {
                    ledger_id: income,
                    amount: -700,
                },
            ],
        )
        .unwrap();
        let saved = db.vouchers(a).unwrap();
        assert_eq!(saved[0].date, "2026-01-03");
        assert_eq!(saved[0].narration, "corrected");
        assert_eq!(saved[0].postings[0].amount, 700);
        assert_eq!(
            db.balances(a)
                .unwrap()
                .iter()
                .find(|b| b.ledger.id == cash)
                .unwrap()
                .amount,
            700
        );
        assert!(db
            .post_voucher(
                a,
                "2026-01-04",
                "",
                &[
                    Posting {
                        ledger_id: cash,
                        amount: 50
                    },
                    Posting {
                        ledger_id: cash,
                        amount: -50
                    }
                ]
            )
            .is_err());
    }
    #[test]
    fn posted_vouchers_survive_reopening_database() {
        let path = std::env::temp_dir().join(format!(
            "optimus-prime-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let company;
        {
            let mut db = Store::open(&path).unwrap();
            company = db.create_company("Example").unwrap();
            let cash = db.create_ledger(company, "Cash", Category::Asset).unwrap();
            let equity = db
                .create_ledger(company, "Capital", Category::Equity)
                .unwrap();
            db.post_voucher(
                company,
                "2026-10-30",
                "",
                &[
                    Posting {
                        ledger_id: cash,
                        amount: 125,
                    },
                    Posting {
                        ledger_id: equity,
                        amount: -125,
                    },
                ],
            )
            .unwrap();
        }
        {
            let db = Store::open(&path).unwrap();
            let balances = db.balances(company).unwrap();
            assert_eq!(balances.iter().map(|b| b.amount).sum::<i64>(), 0);
            assert!(balances
                .iter()
                .any(|b| b.ledger.name == "Cash" && b.amount == 125));
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn amounts_are_exact() {
        assert_eq!(parse_amount("12.30").unwrap(), 1230);
        assert_eq!(parse_amount("0.01").unwrap(), 1);
        assert!(parse_amount("1.001").is_err());
        assert!(parse_amount("-1").is_err());
        assert_eq!(format_amount(-1), "-0.01");
    }
    #[test]
    fn vouchers_are_atomic_balanced_and_company_scoped() {
        let mut db = Store::memory().unwrap();
        let a = db.create_company("A").unwrap();
        let b = db.create_company("B").unwrap();
        let cash = db.create_ledger(a, "Cash", Category::Asset).unwrap();
        let capital = db.create_ledger(a, "Capital", Category::Equity).unwrap();
        let other = db.create_ledger(b, "Other", Category::Asset).unwrap();
        assert!(db
            .post_voucher(
                a,
                "2026-10-30",
                "",
                &[
                    Posting {
                        ledger_id: cash,
                        amount: 100
                    },
                    Posting {
                        ledger_id: capital,
                        amount: -99
                    }
                ]
            )
            .is_err());
        assert!(db
            .post_voucher(
                a,
                "2026-10-30",
                "",
                &[
                    Posting {
                        ledger_id: cash,
                        amount: 100
                    },
                    Posting {
                        ledger_id: other,
                        amount: -100
                    }
                ]
            )
            .is_err());
        assert!(db
            .post_voucher(
                a,
                "2026-02-30",
                "",
                &[
                    Posting {
                        ledger_id: cash,
                        amount: 100
                    },
                    Posting {
                        ledger_id: capital,
                        amount: -100
                    }
                ]
            )
            .is_err());
        assert!(db.balances(a).unwrap().iter().all(|b| b.amount == 0));
        db.post_voucher(
            a,
            "2026-10-30",
            "capital",
            &[
                Posting {
                    ledger_id: cash,
                    amount: 123,
                },
                Posting {
                    ledger_id: capital,
                    amount: -123,
                },
            ],
        )
        .unwrap();
        let balances = db.balances(a).unwrap();
        assert_eq!(
            balances
                .iter()
                .find(|b| b.ledger.id == cash)
                .unwrap()
                .amount,
            123
        );
        assert_eq!(
            balances
                .iter()
                .find(|b| b.ledger.id == capital)
                .unwrap()
                .amount,
            -123
        );
    }
}
