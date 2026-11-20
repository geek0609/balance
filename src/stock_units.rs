//! Compound unit definitions and exact conversion to stock quantities.
use crate::{inventory::parse_quantity, required, Error, Result};
use rusqlite::{params, Connection};
use rust_decimal::{prelude::ToPrimitive, Decimal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompoundUnit {
    pub primary: String,
    pub tail: String,
    /// Number of tail units in one primary unit.
    pub factor: Decimal,
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS stock_unit_compounds (
            unit_id INTEGER PRIMARY KEY REFERENCES stock_units(id),
            primary_unit_id INTEGER NOT NULL REFERENCES stock_units(id),
            tail_unit_id INTEGER NOT NULL REFERENCES stock_units(id),
            stock_unit_id INTEGER NOT NULL REFERENCES stock_units(id),
            factor TEXT NOT NULL,
            CHECK(unit_id != primary_unit_id AND unit_id != tail_unit_id
                AND unit_id != stock_unit_id AND primary_unit_id != tail_unit_id)
        );",
    )?;
    Ok(())
}

pub(crate) fn factor(value: &str) -> Result<Decimal> {
    let value = Decimal::from_str_exact(value.trim())
        .map_err(|_| Error::Validation("Unit conversion must be a decimal number".into()))?;
    if value <= Decimal::ZERO {
        return Err(Error::Validation("Unit conversion must be positive".into()));
    }
    Ok(value)
}

pub(crate) fn insert_compound(
    conn: &Connection,
    company: i64,
    symbol: &str,
    formal_name: &str,
    primary: i64,
    tail: i64,
    factor: Decimal,
) -> Result<i64> {
    let symbol = required(symbol, "Unit symbol")?;
    let primary_is_simple: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stock_units u WHERE u.id = ?1 AND u.company_id = ?2
         AND NOT EXISTS(SELECT 1 FROM stock_unit_compounds c WHERE c.unit_id = u.id))",
        params![primary, company],
        |row| row.get(0),
    )?;
    if primary == tail || !primary_is_simple || factor <= Decimal::ZERO {
        return Err(Error::Validation("Invalid compound unit components".into()));
    }
    let (storage, precision): (i64, u8) = conn.query_row(
        "SELECT COALESCE(c.stock_unit_id, u.id), u.decimal_places FROM stock_units u
         LEFT JOIN stock_unit_compounds c ON c.unit_id = u.id
         WHERE u.id = ?1 AND u.company_id = ?2",
        params![tail, company],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    conn.execute(
        "INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places) VALUES (?1, ?2, ?3, ?4)",
        params![company, symbol, formal_name.trim(), precision],
    )?;
    let unit = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO stock_unit_compounds(unit_id, primary_unit_id, tail_unit_id, stock_unit_id, factor)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![unit, primary, tail, storage, factor.to_string()],
    )?;
    Ok(unit)
}

fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Debug, Clone)]
pub(crate) struct UnitMeasure {
    pub symbol: String,
    pub canonical: String,
    pub precision: u8,
    parts: Vec<UnitPart>,
}

#[derive(Debug, Clone)]
struct UnitPart {
    symbol: String,
    names: Vec<String>,
    factor: Decimal,
}

impl UnitMeasure {
    pub fn simple(symbol: &str, precision: u8) -> Self {
        Self {
            symbol: symbol.to_owned(),
            canonical: symbol.to_owned(),
            precision,
            parts: vec![UnitPart {
                symbol: symbol.to_owned(),
                names: vec![name_key(symbol)],
                factor: Decimal::ONE,
            }],
        }
    }

    pub fn compound(symbol: &str, primary: &Self, tail: &Self, factor: Decimal) -> Result<Self> {
        let primary_symbol = &primary.symbol;
        if primary.parts.len() != 1
            || tail
                .parts
                .iter()
                .any(|part| part.names.contains(&name_key(primary_symbol)))
        {
            return Err(Error::Validation(
                "Compound unit components must be distinct and the primary unit simple".into(),
            ));
        }
        let primary_factor = factor
            .checked_mul(tail.parts[0].factor)
            .ok_or_else(|| Error::Validation("Compound unit conversion is too large".into()))?;
        if primary_factor <= Decimal::ZERO {
            return Err(Error::Validation(
                "Compound unit conversion is below supported precision".into(),
            ));
        }
        let canonical = format!(
            "{primary_symbol} of {} {}",
            factor.normalize(),
            tail.canonical
        );
        let symbol = if symbol.is_empty() {
            canonical.clone()
        } else {
            symbol.to_owned()
        };
        let mut names = vec![
            name_key(primary_symbol),
            name_key(&canonical),
            name_key(&symbol),
        ];
        names.sort_unstable();
        names.dedup();
        if tail
            .parts
            .iter()
            .any(|part| part.names.iter().any(|name| names.contains(name)))
        {
            return Err(Error::Validation(
                "Compound unit names overlap with a component".into(),
            ));
        }
        let mut parts = vec![UnitPart {
            symbol: primary_symbol.clone(),
            names,
            factor: primary_factor,
        }];
        parts.extend_from_slice(&tail.parts);
        Ok(Self {
            symbol,
            canonical,
            precision: tail.precision,
            parts,
        })
    }

    pub fn stock_symbol(&self) -> &str {
        &self.parts.last().expect("unit has a stock measure").symbol
    }

    pub fn factor_for(&self, suffix: &str) -> Result<Decimal> {
        let suffix = name_key(suffix);
        if suffix.is_empty() {
            return Ok(self.parts[0].factor);
        }
        self.parts
            .iter()
            .find_map(|part| part.names.contains(&suffix).then_some(part.factor))
            .ok_or_else(|| {
                Error::Validation("Stock quantity or rate uses an unrelated unit".into())
            })
    }

    pub fn quantity(&self, value: &str) -> Result<i64> {
        let (negative, value) = value
            .trim()
            .strip_prefix('-')
            .map_or((false, value.trim()), |value| (true, value));
        let (number, suffix) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
        if self.parts.len() == 1 {
            self.factor_for(suffix)?;
            let quantity = parse_quantity(number)?;
            return Ok(if negative { -quantity } else { quantity });
        }
        let invalid = || Error::Validation("Invalid compound stock quantity".into());
        let parse = |text: &str| {
            Decimal::from_str_exact(text)
                .ok()
                .filter(|amount| *amount >= Decimal::ZERO)
                .ok_or_else(invalid)
        };
        let amount = if number.contains('-') {
            let mut total = Decimal::ZERO;
            if self.factor_for(suffix)? != self.parts[0].factor {
                return Err(invalid());
            }
            for (index, value) in number.split('-').enumerate() {
                let part = self.parts.get(index).ok_or_else(invalid)?;
                total = parse(value)?
                    .checked_mul(part.factor)
                    .and_then(|part| total.checked_add(part))
                    .ok_or_else(invalid)?;
            }
            total
        } else if let Ok(factor) = self.factor_for(suffix) {
            parse(number)?.checked_mul(factor).ok_or_else(invalid)?
        } else {
            let normalized = name_key(value);
            let mut remaining = normalized.as_str();
            let mut total = Decimal::ZERO;
            let mut next_part = 0;
            while !remaining.is_empty() {
                let (number, after_number) = remaining.split_once(' ').ok_or_else(invalid)?;
                let (index, factor, _, after_unit) = self
                    .parts
                    .iter()
                    .enumerate()
                    .skip(next_part)
                    .flat_map(|(index, part)| {
                        part.names
                            .iter()
                            .map(move |name| (index, part.factor, name))
                    })
                    .filter_map(|(index, factor, name)| {
                        let rest = after_number.strip_prefix(name)?;
                        (rest.is_empty() || rest.starts_with(' ')).then_some((
                            index,
                            factor,
                            name.len(),
                            rest,
                        ))
                    })
                    .max_by_key(|(_, _, name_length, _)| *name_length)
                    .ok_or_else(invalid)?;
                total = parse(number)?
                    .checked_mul(factor)
                    .and_then(|part| total.checked_add(part))
                    .ok_or_else(invalid)?;
                remaining = after_unit.trim_start();
                next_part = index + 1;
            }
            total
        };
        let scaled = amount
            .checked_mul(Decimal::from(1000))
            .ok_or_else(invalid)?;
        let quantity = scaled
            .to_i64()
            .filter(|_| scaled.fract().is_zero())
            .ok_or_else(|| {
                Error::Validation("Compound quantity exceeds stock precision or range".into())
            })?;
        Ok(if negative { -quantity } else { quantity })
    }
}
