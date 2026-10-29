//! Accounting primitives and an isolated SQLite store. Amounts are stored in
//! integer hundredths and parsed with decimal arithmetic, never floating point.

use chrono::NaiveDate;
use rusqlite::{params, Connection};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use std::{collections::HashMap, path::Path, str::FromStr};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Validation(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

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
}
#[derive(Debug, Clone)]
pub struct Posting {
    pub ledger_id: i64,
    /// Positive for debit, negative for credit, in hundredths.
    pub amount: i64,
}
#[derive(Debug, Clone)]
pub struct Balance {
    pub ledger: Ledger,
    /// Debit minus credit, in hundredths.
    pub amount: i64,
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
    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS companies(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
            CREATE TABLE IF NOT EXISTS ledgers(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), name TEXT NOT NULL, category TEXT NOT NULL, UNIQUE(company_id, name));
            CREATE TABLE IF NOT EXISTS vouchers(id INTEGER PRIMARY KEY, company_id INTEGER NOT NULL REFERENCES companies(id), date TEXT NOT NULL, narration TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS postings(id INTEGER PRIMARY KEY, voucher_id INTEGER NOT NULL REFERENCES vouchers(id), ledger_id INTEGER NOT NULL REFERENCES ledgers(id), amount INTEGER NOT NULL CHECK(amount != 0));")?;
        Ok(Self { conn })
    }
    pub fn create_company(&self, name: &str) -> Result<i64> {
        let name = required(name, "Company name")?;
        self.conn
            .execute("INSERT INTO companies(name) VALUES (?1)", [name])?;
        Ok(self.conn.last_insert_rowid())
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
        let name = required(name, "Ledger name")?;
        self.conn.execute(
            "INSERT INTO ledgers(company_id, name, category) VALUES (?1, ?2, ?3)",
            params![company_id, name, category.as_str()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn ledgers(&self, company_id: i64) -> Result<Vec<Ledger>> {
        let mut s = self.conn.prepare(
            "SELECT id, name, category FROM ledgers WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = s
            .query_map([company_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(i64, String, String)>>>()?;
        rows.into_iter()
            .map(|(id, name, category)| {
                Ok(Ledger {
                    id,
                    name,
                    category: Category::parse(&category)?,
                })
            })
            .collect()
    }
    pub fn post_voucher(
        &mut self,
        company_id: i64,
        date: &str,
        narration: &str,
        postings: &[Posting],
    ) -> Result<i64> {
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|_| Error::Validation("Date must be a valid YYYY-MM-DD date".into()))?;
        if postings.len() < 2 || postings.iter().any(|p| p.amount == 0) {
            return Err(Error::Validation(
                "A voucher requires at least two non-zero postings".into(),
            ));
        }
        let total = postings
            .iter()
            .try_fold(0_i64, |sum, p| sum.checked_add(p.amount))
            .ok_or_else(|| Error::Validation("Voucher total is too large".into()))?;
        if total != 0 {
            return Err(Error::Validation("Debits and credits must balance".into()));
        }
        let tx = self.conn.transaction()?;
        for p in postings {
            let found: bool = tx.query_row(
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
        tx.execute(
            "INSERT INTO vouchers(company_id, date, narration) VALUES (?1, ?2, ?3)",
            params![company_id, date, narration.trim()],
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
    pub fn balances(&self, company_id: i64) -> Result<Vec<Balance>> {
        let ledgers = self.ledgers(company_id)?;
        let mut sums = HashMap::<i64, i64>::new();
        let mut s = self.conn.prepare("SELECT p.ledger_id, p.amount FROM postings p JOIN vouchers v ON p.voucher_id = v.id WHERE v.company_id = ?1")?;
        let rows = s.query_map([company_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, amount) = row?;
            let sum = sums.entry(id).or_default();
            *sum = sum
                .checked_add(amount)
                .ok_or_else(|| Error::Validation("Balance exceeds supported range".into()))?;
        }
        Ok(ledgers
            .into_iter()
            .map(|ledger| Balance {
                amount: *sums.get(&ledger.id).unwrap_or(&0),
                ledger,
            })
            .collect())
    }
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
