//! Foreign amount records attached to balanced vouchers in the base currency.
use crate::inventory::ledger_category;
use crate::{
    check_ledgers, parse_date, required, validate_voucher, Category, Error, Posting, Result, Store,
    VoucherKind,
};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use std::str::FromStr;

const RATE_SCALE: i64 = 100_000_000;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE company_currencies(
            company_id INTEGER PRIMARY KEY REFERENCES companies(id),
            base_code TEXT NOT NULL
        );
        CREATE TABLE foreign_currencies(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES company_currencies(company_id),
            code TEXT NOT NULL,
            decimal_places INTEGER NOT NULL CHECK(decimal_places BETWEEN 0 AND 6),
            UNIQUE(company_id, code)
        );
        CREATE TABLE foreign_rates(
            id INTEGER PRIMARY KEY,
            currency_id INTEGER NOT NULL REFERENCES foreign_currencies(id),
            date TEXT NOT NULL,
            rate INTEGER NOT NULL CHECK(rate > 0),
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX foreign_rates_currency ON foreign_rates(currency_id, date, id);
        CREATE TABLE foreign_vouchers(
            voucher_id INTEGER PRIMARY KEY REFERENCES vouchers(id),
            company_id INTEGER NOT NULL REFERENCES companies(id),
            rate_id INTEGER NOT NULL REFERENCES foreign_rates(id)
        );
        CREATE INDEX foreign_vouchers_company ON foreign_vouchers(company_id, voucher_id);
        CREATE TABLE foreign_posting_amounts(
            posting_id INTEGER PRIMARY KEY REFERENCES postings(id),
            foreign_amount INTEGER NOT NULL CHECK(foreign_amount != 0)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ForeignCurrency {
    pub id: i64,
    pub code: String,
    pub decimal_places: u8,
}
#[derive(Debug, Clone)]
pub struct ForeignRate {
    pub id: i64,
    pub date: String,
    pub rate: i64,
    pub reason: String,
    pub recorded_at: String,
}
#[derive(Debug, Clone)]
pub struct ForeignPosting {
    pub ledger_id: i64,
    pub amount: i64,
}
#[derive(Debug, Clone)]
pub struct ForeignVoucherDraft {
    pub currency_id: i64,
    pub kind: VoucherKind,
    pub date: String,
    pub reference: String,
    pub narration: String,
    pub rounding_ledger: Option<i64>,
    pub postings: Vec<ForeignPosting>,
}
#[derive(Debug, Clone)]
pub struct ForeignVoucherLine {
    pub ledger_id: i64,
    pub ledger: String,
    pub foreign_amount: Option<i64>,
    pub base_amount: i64,
}
#[derive(Debug, Clone)]
pub struct ForeignVoucherPlan {
    company: i64,
    draft: ForeignVoucherDraft,
    date: String,
    base_code: String,
    currency: ForeignCurrency,
    rate: ForeignRate,
    lines: Vec<ForeignVoucherLine>,
}
impl ForeignVoucherPlan {
    pub fn date(&self) -> &str {
        &self.date
    }
    pub fn base_code(&self) -> &str {
        &self.base_code
    }
    pub fn currency(&self) -> &ForeignCurrency {
        &self.currency
    }
    pub fn rate(&self) -> &ForeignRate {
        &self.rate
    }
    pub fn lines(&self) -> &[ForeignVoucherLine] {
        &self.lines
    }
}
#[derive(Debug, Clone)]
pub struct ForeignVoucherEntry {
    pub id: i64,
    pub date: String,
    pub reference: String,
    pub kind: VoucherKind,
    pub base_code: String,
    pub currency: ForeignCurrency,
    pub rate: ForeignRate,
}

fn code(input: &str) -> Result<String> {
    let input = input.trim();
    if input.len() != 3 || !input.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(Error::Validation(
            "Enter a three-letter currency code".into(),
        ));
    }
    Ok(input.to_ascii_uppercase())
}

fn scaled(text: &str, scale: i64) -> Result<i64> {
    let amount = Decimal::from_str(text.trim())
        .map_err(|_| Error::Validation("Enter a decimal number".into()))?;
    let scaled = amount
        .checked_mul(Decimal::new(scale, 0))
        .ok_or_else(|| Error::Validation("Amount is too large".into()))?;
    if scaled < Decimal::ZERO || !scaled.fract().is_zero() {
        return Err(Error::Validation(
            "Amount must be non-negative and fit the allowed precision".into(),
        ));
    }
    scaled
        .to_i64()
        .ok_or_else(|| Error::Validation("Amount is too large".into()))
}

pub fn parse_foreign_amount(text: &str, places: u8) -> Result<i64> {
    if places > 6 {
        return Err(Error::Validation("Unsupported currency precision".into()));
    }
    scaled(text, 10_i64.pow(places.into()))
}

pub fn format_foreign_amount(amount: i64, places: u8) -> String {
    Decimal::new(amount, u32::from(places.min(6))).to_string()
}

pub fn parse_exchange_rate(text: &str) -> Result<i64> {
    let rate = scaled(text, RATE_SCALE)?;
    if rate == 0 {
        return Err(Error::Validation("Exchange rate must be positive".into()));
    }
    Ok(rate)
}

pub fn format_exchange_rate(rate: i64) -> String {
    Decimal::new(rate, 8).normalize().to_string()
}

fn currency(conn: &Connection, company: i64, id: i64) -> Result<ForeignCurrency> {
    conn.query_row(
        "SELECT id, code, decimal_places FROM foreign_currencies WHERE company_id = ?1 AND id = ?2",
        params![company, id],
        |row| {
            Ok(ForeignCurrency {
                id: row.get(0)?,
                code: row.get(1)?,
                decimal_places: row.get(2)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::Validation("Foreign currency not found in this company".into()))
}

fn base_code(conn: &Connection, company: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT base_code FROM company_currencies WHERE company_id = ?1",
            [company],
            |row| row.get(0),
        )
        .optional()?)
}

fn load_rates(conn: &Connection, currency: i64) -> Result<Vec<ForeignRate>> {
    let mut stmt = conn.prepare(
        "SELECT id,date,rate,reason,recorded_at FROM foreign_rates WHERE currency_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([currency], |row| {
        Ok(ForeignRate {
            id: row.get(0)?,
            date: row.get(1)?,
            rate: row.get(2)?,
            reason: row.get(3)?,
            recorded_at: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn convert(amount: i64, rate: i64, places: u8) -> Result<i64> {
    // Rates are base currency units per one foreign currency unit. Ledger
    // postings use base hundredths, so two decimal places cancel from scale.
    let denominator = 10_i128.pow(u32::from(places)) * i128::from(RATE_SCALE / 100);
    let numerator = i128::from(amount).abs() * i128::from(rate);
    let value = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded =
        (value + i128::from(remainder >= denominator - remainder)) * i128::from(amount.signum());
    let supported = i64::try_from(rounded)
        .ok()
        .filter(|value| *value != 0 && *value != i64::MIN);
    supported.ok_or_else(|| {
        Error::Validation("A converted line is zero or exceeds the supported amount".into())
    })
}

fn prepare(
    conn: &Connection,
    company: i64,
    draft: &ForeignVoucherDraft,
) -> Result<ForeignVoucherPlan> {
    let date = parse_date(&draft.date)?;
    required(&draft.reference, "Voucher reference")?;
    if !matches!(
        draft.kind,
        VoucherKind::Journal | VoucherKind::Transfer | VoucherKind::Receipt | VoucherKind::Payment
    ) {
        return Err(Error::Validation(
            "Choose a journal, transfer, receipt or payment".into(),
        ));
    }
    if crate::periods::cutoff(conn, company)?.is_some_and(|cutoff| date <= cutoff) {
        return Err(Error::Validation(
            crate::periods::LOCKED_PERIOD_MESSAGE.into(),
        ));
    }
    let base_code = base_code(conn, company)?
        .ok_or_else(|| Error::Validation("Set the base currency first".into()))?;
    let currency = currency(conn, company, draft.currency_id)?;
    let mut latest: Option<(NaiveDate, ForeignRate)> = None;
    for rate in load_rates(conn, currency.id)? {
        let effective = parse_date(&rate.date)?;
        if effective <= date
            && latest
                .as_ref()
                .is_none_or(|(last, previous)| (effective, rate.id) > (*last, previous.id))
        {
            latest = Some((effective, rate));
        }
    }
    let (_, rate) = latest
        .ok_or_else(|| Error::Validation("No rate is effective on the voucher date".into()))?;
    if draft.postings.len() < 2
        || draft
            .postings
            .iter()
            .any(|row| row.amount == 0 || row.amount == i64::MIN)
        || draft
            .postings
            .iter()
            .map(|row| i128::from(row.amount))
            .sum::<i128>()
            != 0
        || draft
            .postings
            .iter()
            .all(|row| row.ledger_id == draft.postings[0].ledger_id)
    {
        return Err(Error::Validation(
            "Foreign amounts must balance across at least two ledgers".into(),
        ));
    }
    let mut lines = Vec::with_capacity(draft.postings.len() + 1);
    let mut base_total = 0_i128;
    let ledger_name = |ledger: i64| -> Result<String> {
        conn.query_row(
            "SELECT name FROM ledgers WHERE company_id = ?1 AND id = ?2",
            params![company, ledger],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::Validation("Ledger not found in the selected company".into()))
    };
    for row in &draft.postings {
        let base_amount = convert(row.amount, rate.rate, currency.decimal_places)?;
        base_total += i128::from(base_amount);
        lines.push(ForeignVoucherLine {
            ledger_id: row.ledger_id,
            ledger: ledger_name(row.ledger_id)?,
            foreign_amount: Some(row.amount),
            base_amount,
        });
    }
    if base_total != 0 {
        let ledger_id = draft.rounding_ledger.ok_or_else(|| {
            Error::Validation(format!(
                "Conversion leaves a {} rounding difference. Select a rounding ledger.",
                crate::format_amount(i64::try_from(base_total.abs()).unwrap_or(i64::MAX))
            ))
        })?;
        if draft.postings.iter().any(|row| row.ledger_id == ledger_id)
            || !matches!(
                ledger_category(conn, company, ledger_id)?,
                Category::Income | Category::Expense
            )
        {
            return Err(Error::Validation(
                "Round using a separate income or expense ledger".into(),
            ));
        }
        let amount = i64::try_from(-base_total)
            .map_err(|_| Error::Validation("Rounding amount is too large".into()))?;
        lines.push(ForeignVoucherLine {
            ledger_id,
            ledger: ledger_name(ledger_id)?,
            foreign_amount: None,
            base_amount: amount,
        });
    }
    let postings = lines
        .iter()
        .map(|line| Posting {
            ledger_id: line.ledger_id,
            amount: line.base_amount,
        })
        .collect::<Vec<_>>();
    let date = date.format("%Y-%m-%d").to_string();
    validate_voucher(&date, &postings)?;
    check_ledgers(conn, company, &postings)?;
    crate::opening::check_no_control_postings(conn, company, &postings)?;
    Ok(ForeignVoucherPlan {
        company,
        draft: draft.clone(),
        date,
        base_code,
        currency,
        rate,
        lines,
    })
}

pub(crate) fn copy_foreign_reversal(
    conn: &Connection,
    company: i64,
    original: i64,
    reversal: i64,
) -> Result<()> {
    let added = conn.execute(
        "INSERT INTO foreign_vouchers(voucher_id, company_id, rate_id)
         SELECT ?3, company_id, rate_id FROM foreign_vouchers WHERE company_id = ?1 AND voucher_id = ?2",
        params![company, original, reversal],
    )?;
    if added != 0 {
        conn.execute(
            "WITH old AS (
                SELECT p.id, ROW_NUMBER() OVER (ORDER BY p.id) AS ord FROM postings p WHERE p.voucher_id = ?1),
             counter AS (
                SELECT p.id, ROW_NUMBER() OVER (ORDER BY p.id) AS ord FROM postings p WHERE p.voucher_id = ?2)
             INSERT INTO foreign_posting_amounts(posting_id, foreign_amount)
             SELECT counter.id, -f.foreign_amount FROM old JOIN counter ON counter.ord = old.ord
             JOIN foreign_posting_amounts f ON f.posting_id = old.id",
            params![original, reversal],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn company_base_currency(&self, company: i64) -> Result<Option<String>> {
        base_code(&self.conn, company)
    }

    pub fn set_company_base_currency(&mut self, company: i64, value: &str) -> Result<()> {
        let value = code(value)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if base_code(&tx, company)?.as_deref() == Some(&value) {
            return Ok(());
        }
        let locked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM foreign_currencies WHERE company_id = ?1)",
            [company],
            |row| row.get(0),
        )?;
        if locked {
            return Err(Error::Validation(
                "Base currency is fixed once foreign currencies are added".into(),
            ));
        }
        tx.execute(
            "INSERT INTO company_currencies(company_id, base_code) VALUES (?1, ?2)
             ON CONFLICT(company_id) DO UPDATE SET base_code = excluded.base_code",
            params![company, value],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_foreign_currency(&mut self, company: i64, value: &str, places: u8) -> Result<i64> {
        let value = code(value)?;
        if places > 6 {
            return Err(Error::Validation("Choose up to six decimal places".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let base = base_code(&tx, company)?
            .ok_or_else(|| Error::Validation("Set the base currency first".into()))?;
        if value == base {
            return Err(Error::Validation(
                "The foreign currency must differ from the base currency".into(),
            ));
        }
        let wrote = tx.execute(
            "INSERT INTO foreign_currencies(company_id,code,decimal_places) VALUES (?1,?2,?3)
             ON CONFLICT(company_id,code) DO NOTHING",
            params![company, value, places],
        )?;
        if wrote == 0 {
            return Err(Error::Validation("This currency already exists".into()));
        }
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn foreign_currencies(&self, company: i64) -> Result<Vec<ForeignCurrency>> {
        let mut stmt = self.conn.prepare("SELECT id,code,decimal_places FROM foreign_currencies WHERE company_id = ?1 ORDER BY code")?;
        let rows = stmt.query_map([company], |row| {
            Ok(ForeignCurrency {
                id: row.get(0)?,
                code: row.get(1)?,
                decimal_places: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn record_foreign_rate(
        &mut self,
        company: i64,
        id: i64,
        date: &str,
        rate: i64,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let reason = required(reason, "Reason for setting the rate")?;
        if rate <= 0 {
            return Err(Error::Validation("Exchange rate must be positive".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        currency(&tx, company, id)?;
        tx.execute(
            "INSERT INTO foreign_rates(currency_id,date,rate,reason) VALUES (?1,?2,?3,?4)",
            params![id, date, rate, reason],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn foreign_rates(&self, company: i64, id: i64) -> Result<Vec<ForeignRate>> {
        currency(&self.conn, company, id)?;
        load_rates(&self.conn, id)
    }

    pub fn preview_foreign_voucher(
        &self,
        company: i64,
        draft: &ForeignVoucherDraft,
    ) -> Result<ForeignVoucherPlan> {
        let tx = self.conn.unchecked_transaction()?;
        let plan = prepare(&tx, company, draft)?;
        tx.commit()?;
        Ok(plan)
    }

    pub fn post_foreign_voucher(&mut self, company: i64, plan: &ForeignVoucherPlan) -> Result<i64> {
        if company != plan.company {
            return Err(Error::Validation(
                "Preview the voucher for this company first".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = prepare(&tx, company, &plan.draft)?;
        if current.rate.id != plan.rate.id {
            return Err(Error::Validation(
                "Exchange rate changed. Preview the voucher again.".into(),
            ));
        }
        tx.execute(
            "INSERT INTO vouchers(company_id,kind,reference,date,narration) VALUES (?1,?2,?3,?4,?5)",
            params![company, current.draft.kind.as_str(), current.draft.reference.trim(), current.date, current.draft.narration.trim()],
        )?;
        let voucher = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO foreign_vouchers(voucher_id,company_id,rate_id) VALUES (?1,?2,?3)",
            params![voucher, company, current.rate.id],
        )?;
        for line in current.lines {
            tx.execute(
                "INSERT INTO postings(voucher_id,ledger_id,amount) VALUES (?1,?2,?3)",
                params![voucher, line.ledger_id, line.base_amount],
            )?;
            if let Some(amount) = line.foreign_amount {
                tx.execute(
                    "INSERT INTO foreign_posting_amounts(posting_id,foreign_amount) VALUES (?1,?2)",
                    params![tx.last_insert_rowid(), amount],
                )?;
            }
        }
        tx.commit()?;
        Ok(voucher)
    }

    pub fn is_foreign_voucher(&self, company: i64, voucher: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM foreign_vouchers WHERE company_id = ?1 AND voucher_id = ?2)",
            params![company, voucher], |row| row.get(0),
        )?)
    }

    pub fn foreign_vouchers(&self, company: i64) -> Result<Vec<ForeignVoucherEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.id,v.date,v.reference,v.kind,b.base_code,c.id,c.code,c.decimal_places,r.id,r.date,r.rate,r.reason,r.recorded_at
             FROM foreign_vouchers f JOIN vouchers v ON v.id = f.voucher_id AND v.company_id = f.company_id
             JOIN company_currencies b ON b.company_id = f.company_id JOIN foreign_rates r ON r.id = f.rate_id
             JOIN foreign_currencies c ON c.id = r.currency_id AND c.company_id = f.company_id
             WHERE f.company_id = ?1 ORDER BY v.date DESC,v.id DESC",
        )?;
        let mut rows = stmt.query([company])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            result.push(ForeignVoucherEntry {
                id: row.get(0)?,
                date: row.get(1)?,
                reference: row.get(2)?,
                kind: VoucherKind::parse(&row.get::<_, String>(3)?)?,
                base_code: row.get(4)?,
                currency: ForeignCurrency {
                    id: row.get(5)?,
                    code: row.get(6)?,
                    decimal_places: row.get(7)?,
                },
                rate: ForeignRate {
                    id: row.get(8)?,
                    date: row.get(9)?,
                    rate: row.get(10)?,
                    reason: row.get(11)?,
                    recorded_at: row.get(12)?,
                },
            });
        }
        Ok(result)
    }

    pub fn foreign_voucher_lines(
        &self,
        company: i64,
        voucher: i64,
    ) -> Result<Vec<ForeignVoucherLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.ledger_id,l.name,a.foreign_amount,p.amount FROM foreign_vouchers f
             JOIN postings p ON p.voucher_id = f.voucher_id JOIN ledgers l ON l.id = p.ledger_id
             LEFT JOIN foreign_posting_amounts a ON a.posting_id = p.id
             WHERE f.company_id = ?1 AND f.voucher_id = ?2 ORDER BY p.id",
        )?;
        let rows = stmt.query_map(params![company, voucher], |row| {
            Ok(ForeignVoucherLine {
                ledger_id: row.get(0)?,
                ledger: row.get(1)?,
                foreign_amount: row.get(2)?,
                base_amount: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
