//! Configurable rates for calculating and retaining an invoice tax breakdown.
use crate::inventory::{ledger_category, InvoiceKind};
use crate::taxes::InvoiceTaxLine;
use crate::{parse_date, required, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use std::{collections::HashSet, str::FromStr};

const RATE_SCALE: i64 = 1_000_000;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE tax_profiles(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            kind TEXT NOT NULL CHECK(kind IN ('Sale', 'Purchase')),
            name TEXT NOT NULL,
            name_key TEXT NOT NULL,
            UNIQUE(company_id, kind, name_key)
        );
        CREATE TABLE tax_profile_versions(
            id INTEGER PRIMARY KEY,
            profile_id INTEGER NOT NULL REFERENCES tax_profiles(id),
            effective_on TEXT NOT NULL,
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX tax_profile_versions_profile ON tax_profile_versions(profile_id, effective_on, id);
        CREATE TABLE tax_profile_rates(
            version_id INTEGER NOT NULL REFERENCES tax_profile_versions(id),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            rate INTEGER NOT NULL CHECK(rate BETWEEN 1 AND 1000000),
            PRIMARY KEY(version_id, ledger_id)
        );
        CREATE TABLE invoice_tax_calculations(
            invoice_id INTEGER PRIMARY KEY REFERENCES invoices(id),
            version_id INTEGER NOT NULL REFERENCES tax_profile_versions(id),
            taxable_amount INTEGER NOT NULL CHECK(taxable_amount > 0),
            whole_invoice INTEGER NOT NULL CHECK(whole_invoice IN (0, 1))
        );
        CREATE TABLE invoice_revision_tax_calculations(
            revision_id INTEGER PRIMARY KEY REFERENCES invoice_revisions(id),
            version_id INTEGER NOT NULL REFERENCES tax_profile_versions(id),
            taxable_amount INTEGER NOT NULL CHECK(taxable_amount > 0),
            whole_invoice INTEGER NOT NULL CHECK(whole_invoice IN (0, 1))
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct TaxRate {
    pub ledger_id: i64,
    /// Parts per million of the taxable amount. One percent is 10,000.
    pub rate: i64,
}

#[derive(Debug, Clone)]
pub struct TaxProfileDraft {
    pub kind: InvoiceKind,
    pub name: String,
    pub effective_on: String,
    pub reason: String,
    pub rates: Vec<TaxRate>,
}

#[derive(Debug, Clone)]
pub struct TaxProfile {
    pub id: i64,
    pub name: String,
    pub kind: InvoiceKind,
}

#[derive(Debug, Clone)]
pub struct TaxProfileVersion {
    pub id: i64,
    pub effective_on: String,
    pub reason: String,
    pub recorded_at: String,
    pub rates: Vec<TaxRate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaxCalculation {
    pub version_id: i64,
    pub taxable_amount: i64,
    pub whole_invoice: bool,
}

#[derive(Debug, Clone)]
pub struct TaxCalculationDetail {
    pub calculation: TaxCalculation,
    pub profile_id: i64,
    pub profile: String,
    pub effective_on: String,
}

pub fn parse_tax_rate(text: &str) -> Result<i64> {
    let decimal = Decimal::from_str(text.trim())
        .map_err(|_| Error::Validation("Enter a decimal tax percentage".into()))?;
    let scaled = decimal
        .checked_mul(Decimal::new(10_000, 0))
        .ok_or_else(|| Error::Validation("Tax percentage is too large".into()))?;
    if !scaled.fract().is_zero() {
        return Err(Error::Validation(
            "Tax percentage can have up to four decimal places".into(),
        ));
    }
    scaled
        .to_i64()
        .filter(|value| (1..=RATE_SCALE).contains(value))
        .ok_or_else(|| {
            Error::Validation("Tax percentage must be greater than zero and at most 100".into())
        })
}

pub fn format_tax_rate(rate: i64) -> String {
    Decimal::new(rate, 4).normalize().to_string()
}

fn version_rates(conn: &Connection, version: i64) -> Result<Vec<TaxRate>> {
    let mut stmt = conn.prepare(
        "SELECT ledger_id, rate FROM tax_profile_rates WHERE version_id = ?1 ORDER BY ledger_id",
    )?;
    let rows = stmt.query_map([version], |row| {
        Ok(TaxRate {
            ledger_id: row.get(0)?,
            rate: row.get(1)?,
        })
    })?;
    let rates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if rates.is_empty() {
        return Err(Error::Validation(
            "Tax rate version has no components".into(),
        ));
    }
    Ok(rates)
}

fn calculate(rates: &[TaxRate], taxable: i64) -> Result<Vec<InvoiceTaxLine>> {
    if taxable <= 0 {
        return Err(Error::Validation("Enter a positive taxable amount".into()));
    }
    let scale = i128::from(RATE_SCALE);
    let mut result = Vec::with_capacity(rates.len());
    for rate in rates {
        let amount = (i128::from(taxable) * i128::from(rate.rate) + scale / 2) / scale;
        if amount > 0 {
            result.push(InvoiceTaxLine {
                ledger_id: rate.ledger_id,
                amount: i64::try_from(amount)
                    .map_err(|_| Error::Validation("Tax amount is too large".into()))?,
            });
        }
    }
    Ok(result)
}

pub(crate) fn validate_invoice_calculation(
    conn: &Connection,
    company: i64,
    kind: InvoiceKind,
    date: NaiveDate,
    before_tax: i128,
    context: TaxCalculation,
    taxes: &[InvoiceTaxLine],
) -> Result<()> {
    let effective: String = conn.query_row(
        "SELECT v.effective_on FROM tax_profile_versions v JOIN tax_profiles p ON p.id = v.profile_id
         WHERE p.company_id = ?1 AND p.kind = ?2 AND v.id = ?3",
        params![company, kind.as_str(), context.version_id], |row| row.get(0),
    ).optional()?.ok_or_else(|| Error::Validation("The tax profile no longer matches the company or invoice type".into()))?;
    if parse_date(&effective)? > date {
        return Err(Error::Validation(
            "The tax rate version takes effect after the invoice date".into(),
        ));
    }
    let base = i128::from(context.taxable_amount);
    if base > before_tax || (context.whole_invoice && base != before_tax) {
        return Err(Error::Validation(
            "The invoice amount changed. Recalculate its taxes or use manual amounts.".into(),
        ));
    }
    let expected = calculate(
        &version_rates(conn, context.version_id)?,
        context.taxable_amount,
    )?;
    let mut input = taxes.to_vec();
    input.sort_by_key(|line| line.ledger_id);
    if input != expected {
        return Err(Error::Validation(
            "Tax amounts differ from the stored calculation".into(),
        ));
    }
    Ok(())
}

pub(crate) fn write_calculation(
    conn: &Connection,
    invoice: i64,
    calculation: Option<TaxCalculation>,
) -> Result<()> {
    if let Some(calculation) = calculation {
        conn.execute(
            "INSERT INTO invoice_tax_calculations(invoice_id, version_id, taxable_amount, whole_invoice)
             VALUES (?1, ?2, ?3, ?4)",
            params![invoice, calculation.version_id, calculation.taxable_amount, calculation.whole_invoice],
        )?;
    }
    Ok(())
}

pub(crate) fn calculation_detail(
    conn: &Connection,
    id: i64,
    revision: bool,
) -> Result<Option<TaxCalculationDetail>> {
    let (table, id_column) = if revision {
        ("invoice_revision_tax_calculations", "revision_id")
    } else {
        ("invoice_tax_calculations", "invoice_id")
    };
    let sql = format!(
        "SELECT t.version_id, t.taxable_amount, t.whole_invoice, p.id, p.name, v.effective_on
         FROM {table} t JOIN tax_profile_versions v ON v.id = t.version_id
         JOIN tax_profiles p ON p.id = v.profile_id WHERE t.{id_column} = ?1"
    );
    Ok(conn
        .query_row(&sql, [id], |row| {
            Ok(TaxCalculationDetail {
                calculation: TaxCalculation {
                    version_id: row.get(0)?,
                    taxable_amount: row.get(1)?,
                    whole_invoice: row.get(2)?,
                },
                profile_id: row.get(3)?,
                profile: row.get(4)?,
                effective_on: row.get(5)?,
            })
        })
        .optional()?)
}

impl Store {
    /// Add a rate version to a named sale or purchase profile.
    pub fn save_tax_profile_version(
        &mut self,
        company: i64,
        draft: &TaxProfileDraft,
    ) -> Result<i64> {
        let name = required(&draft.name, "Tax profile name")?;
        let reason = required(&draft.reason, "Reason for saving these rates")?;
        let date = parse_date(&draft.effective_on)?
            .format("%Y-%m-%d")
            .to_string();
        if draft.rates.is_empty() {
            return Err(Error::Validation("Enter at least one tax rate".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut ledgers = HashSet::new();
        for rate in &draft.rates {
            if !(1..=RATE_SCALE).contains(&rate.rate) || !ledgers.insert(rate.ledger_id) {
                return Err(Error::Validation(
                    "Enter a valid percentage once per tax ledger".into(),
                ));
            }
            if !matches!(
                ledger_category(&tx, company, rate.ledger_id)?,
                Category::Asset | Category::Liability
            ) {
                return Err(Error::Validation(
                    "Tax ledgers must be assets or liabilities".into(),
                ));
            }
        }
        let key = name.to_lowercase();
        tx.execute(
            "INSERT INTO tax_profiles(company_id, kind, name, name_key) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(company_id, kind, name_key) DO NOTHING",
            params![company, draft.kind.as_str(), name, key],
        )?;
        let profile: i64 = tx.query_row(
            "SELECT id FROM tax_profiles WHERE company_id = ?1 AND kind = ?2 AND name_key = ?3",
            params![company, draft.kind.as_str(), key],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO tax_profile_versions(profile_id, effective_on, reason) VALUES (?1, ?2, ?3)",
            params![profile, date, reason],
        )?;
        let version = tx.last_insert_rowid();
        for rate in &draft.rates {
            tx.execute(
                "INSERT INTO tax_profile_rates(version_id, ledger_id, rate) VALUES (?1, ?2, ?3)",
                params![version, rate.ledger_id, rate.rate],
            )?;
        }
        tx.commit()?;
        Ok(version)
    }

    pub fn tax_profiles(&self, company: i64) -> Result<Vec<TaxProfile>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind FROM tax_profiles WHERE company_id = ?1 ORDER BY name, kind, id",
        )?;
        let mut rows = stmt.query([company])?;
        let mut profiles = Vec::new();
        while let Some(row) = rows.next()? {
            profiles.push(TaxProfile {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: InvoiceKind::parse(&row.get::<_, String>(2)?)?,
            });
        }
        Ok(profiles)
    }

    pub fn tax_profile_versions(
        &self,
        company: i64,
        profile: i64,
    ) -> Result<Vec<TaxProfileVersion>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.id, v.effective_on, v.reason, v.recorded_at FROM tax_profile_versions v
             JOIN tax_profiles p ON p.id = v.profile_id WHERE p.company_id = ?1 AND p.id = ?2 ORDER BY v.id DESC",
        )?;
        let mut rows = stmt.query(params![company, profile])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            result.push(TaxProfileVersion {
                id,
                effective_on: row.get(1)?,
                reason: row.get(2)?,
                recorded_at: row.get(3)?,
                rates: version_rates(&self.conn, id)?,
            });
        }
        Ok(result)
    }

    pub fn calculate_invoice_tax(
        &self,
        company: i64,
        profile: i64,
        kind: InvoiceKind,
        date: &str,
        taxable_amount: i64,
        whole_invoice: bool,
    ) -> Result<(TaxCalculation, Vec<InvoiceTaxLine>)> {
        let date = parse_date(date)?;
        let mut stmt = self.conn.prepare(
            "SELECT v.id, v.effective_on FROM tax_profile_versions v JOIN tax_profiles p ON p.id = v.profile_id
             WHERE p.company_id = ?1 AND p.id = ?2 AND p.kind = ?3",
        )?;
        let mut rows = stmt.query(params![company, profile, kind.as_str()])?;
        let mut chosen = None;
        while let Some(row) = rows.next()? {
            let effective = parse_date(&row.get::<_, String>(1)?)?;
            let choice = (effective, row.get::<_, i64>(0)?);
            if effective <= date && chosen.is_none_or(|old| choice > old) {
                chosen = Some(choice);
            }
        }
        let (_, version_id) = chosen.ok_or_else(|| {
            Error::Validation("No rate version applies to this invoice date and type".into())
        })?;
        let context = TaxCalculation {
            version_id,
            taxable_amount,
            whole_invoice,
        };
        let taxes = calculate(&version_rates(&self.conn, version_id)?, taxable_amount)?;
        Ok((context, taxes))
    }
}
