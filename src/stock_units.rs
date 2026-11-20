//! Compound unit definitions and exact conversion to stock quantities.
use crate::{
    inventory::{parse_quantity, InvoiceRateBasis},
    required, Error, Result, Store,
};
use rusqlite::{params, Connection};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

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

pub(crate) fn unit_key(name: &str) -> String {
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
                names: vec![unit_key(symbol)],
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
                .any(|part| part.names.contains(&unit_key(primary_symbol)))
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
            unit_key(primary_symbol),
            unit_key(&canonical),
            unit_key(&symbol),
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
        let suffix = unit_key(suffix);
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
            let normalized = unit_key(value);
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

    pub fn rate_basis(&self, unit: &str) -> Result<Option<InvoiceRateBasis>> {
        let unit = required(unit, "Item rate unit")?;
        if unit_key(unit) == unit_key(self.stock_symbol()) {
            return Ok(None);
        }
        let quantity = self
            .factor_for(unit)?
            .checked_mul(Decimal::from(1000))
            .filter(|quantity| quantity.fract().is_zero())
            .and_then(|quantity| quantity.to_i64())
            .filter(|quantity| *quantity > 0)
            .ok_or_else(|| {
                Error::Validation("Price unit exceeds supported stock precision or range".into())
            })?;
        Ok(Some(InvoiceRateBasis {
            quantity,
            unit: unit.to_owned(),
        }))
    }
}

pub(crate) type Measures = HashMap<String, Arc<UnitMeasure>>;

pub(crate) fn resolve_compound(
    symbol: &str,
    compound: &CompoundUnit,
    known: &Measures,
) -> Result<Option<UnitMeasure>> {
    let (Some(primary), Some(tail)) = (
        known.get(&unit_key(&compound.primary)),
        known.get(&unit_key(&compound.tail)),
    ) else {
        return Ok(None);
    };
    UnitMeasure::compound(symbol, primary, tail, compound.factor).map(Some)
}

pub(crate) fn index_measure(known: &mut Measures, measure: UnitMeasure) -> Result<()> {
    let symbol = unit_key(&measure.symbol);
    let canonical = unit_key(&measure.canonical);
    if known.contains_key(&symbol) || known.contains_key(&canonical) {
        return Err(Error::Validation(format!(
            "Unit {} is ambiguous",
            measure.symbol
        )));
    }
    let measure = Arc::new(measure);
    known.insert(symbol, Arc::clone(&measure));
    known.insert(canonical, measure);
    Ok(())
}

pub(crate) fn saved_measures(units: &[crate::inventory::StockUnit]) -> Result<Measures> {
    let mut ordered = units.iter().collect::<Vec<_>>();
    // A compound's referenced units are inserted before the compound itself.
    ordered.sort_by_key(|unit| unit.id);
    let symbols: HashSet<_> = units.iter().map(|unit| unit_key(&unit.symbol)).collect();
    if symbols.len() != units.len() {
        return Err(Error::Validation(
            "Stored unit symbols are ambiguous".into(),
        ));
    }
    let mut aliases = HashSet::new();
    let mut known = Measures::new();
    for unit in ordered {
        let measure = match &unit.compound {
            None => UnitMeasure::simple(&unit.symbol, unit.decimal_places),
            Some(compound) => resolve_compound(&unit.symbol, compound, &known)?
                .filter(|measure| measure.precision == unit.decimal_places)
                .ok_or_else(|| {
                    Error::Validation(format!(
                        "Stored unit {} has invalid components",
                        unit.symbol
                    ))
                })?,
        };
        let symbol = unit_key(&measure.symbol);
        let canonical = unit_key(&measure.canonical);
        let measure = Arc::new(measure);
        known.insert(symbol, Arc::clone(&measure));
        // Existing symbols remain authoritative when a generated alias collides.
        if !symbols.contains(&canonical) {
            if aliases.insert(canonical.clone()) {
                known.insert(canonical, measure);
            } else {
                known.remove(&canonical);
            }
        }
    }
    Ok(known)
}

impl Store {
    /// Create a unit with a fixed conversion to a smaller unit.
    /// A blank symbol uses the conversion description as its name.
    pub fn create_compound_stock_unit(
        &self,
        company: i64,
        symbol: &str,
        primary: i64,
        tail: i64,
        conversion: &str,
    ) -> Result<i64> {
        let factor = factor(conversion)?;
        let tx = self.conn.unchecked_transaction()?;
        let units = self.stock_units(company)?;
        let mut measures = saved_measures(&units)?;
        let measure_for = |id| {
            units
                .iter()
                .find(|unit| unit.id == id)
                .and_then(|unit| measures.get(&unit_key(&unit.symbol)))
                .ok_or_else(|| {
                    Error::Validation("Select two units from the current company".into())
                })
        };
        let measure = UnitMeasure::compound(
            symbol.trim(),
            measure_for(primary)?,
            measure_for(tail)?,
            factor,
        )?;
        let symbol = measure.symbol.clone();
        index_measure(&mut measures, measure)?;
        let id = insert_compound(&tx, company, &symbol, "", primary, tail, factor)?;
        tx.commit()?;
        Ok(id)
    }

    /// Invoice pricing choices by stock unit ID. The stock unit itself is the default.
    pub fn invoice_price_units(&self, company: i64) -> Result<HashMap<i64, Vec<InvoiceRateBasis>>> {
        let units = self.stock_units(company)?;
        let measures = saved_measures(&units)?;
        let mut result = HashMap::new();
        for unit in units.iter().filter(|unit| unit.compound.is_some()) {
            let measure = &measures[&unit_key(&unit.symbol)];
            let bases = measure
                .parts
                .iter()
                .filter_map(|part| measure.rate_basis(&part.symbol).ok().flatten())
                .collect::<Vec<_>>();
            if !bases.is_empty() {
                result.insert(unit.id, bases);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        inventory::{InvoiceDiscount, InvoiceDraft, InvoiceEntryMode, InvoiceKind, InvoiceLine},
        Category,
    };

    #[test]
    fn native_compound_units_offer_invoice_prices_in_each_component() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Packaging units").unwrap();
        let other = store.create_company("Another company").unwrap();
        let piece = store
            .create_stock_unit(company, "pcs", "Pieces", 0)
            .unwrap();
        let box_unit = store.create_stock_unit(company, "Box", "", 0).unwrap();
        let case = store.create_stock_unit(company, "Case", "", 0).unwrap();
        let foreign = store.create_stock_unit(other, "pcs", "Pieces", 0).unwrap();
        assert!(store
            .create_compound_stock_unit(company, "", box_unit, foreign, "12")
            .is_err());
        assert!(store
            .create_compound_stock_unit(company, "", box_unit, piece, "0")
            .is_err());
        let pack = store
            .create_compound_stock_unit(company, "", box_unit, piece, "12")
            .unwrap();
        assert!(store
            .create_compound_stock_unit(company, "Retail box", box_unit, piece, "12.00")
            .is_err());
        assert!(store
            .create_compound_stock_unit(company, "", box_unit, pack, "2")
            .is_err());
        let carton = store
            .create_compound_stock_unit(company, "Shipping  case", case, pack, "2")
            .unwrap();
        assert_eq!(store.stock_units(company).unwrap().len(), 5);

        let preview = store
            .preview_xml_import(
                company,
                br#"<ENVELOPE><BODY><DATA><MESSAGE>
            <STOCKITEM><NAME>Parts</NAME><BASEUNITS>Shipping case</BASEUNITS></STOCKITEM>
        </MESSAGE></DATA></BODY></ENVELOPE>"#,
            )
            .unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        store.apply_xml_import(&preview).unwrap();
        let item = store.stock_items(company).unwrap().remove(0);
        assert_eq!((item.unit_id, item.unit.as_str()), (carton, "pcs"));
        let options = store.invoice_price_units(company).unwrap();
        assert_eq!(
            options[&carton]
                .iter()
                .map(|basis| (basis.unit.as_str(), basis.quantity))
                .collect::<Vec<_>>(),
            [("Case", 24000), ("Box", 12000)]
        );
        assert!(!options.contains_key(&piece));
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let trade = store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let location = store.stock_locations(company).unwrap().remove(0).id;
        let draft = InvoiceDraft {
            kind: InvoiceKind::Purchase,
            entry_mode: InvoiceEntryMode::Invoice,
            date: "2026-05-01".into(),
            due_date: None,
            credit_days: None,
            reference: "PACK-01".into(),
            external_reference: Default::default(),
            bill_reference: None,
            bill_splits: Vec::new(),
            narration: String::new(),
            party_ledger_id: supplier,
            trade_ledger_id: trade,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: Vec::new(),
            tax_calculation: None,
            lines: vec![InvoiceLine {
                item_id: item.id,
                location_id: location,
                batch_id: None,
                trade_ledger_id: None,
                extra_allocations: Vec::new(),
                quantity: 25000,
                billed_quantity: Some(24000),
                unit_price: 10500,
                rate_basis: Some(options[&carton][0].clone()),
                discount: InvoiceDiscount::None,
            }],
            adjustments: Vec::new(),
        };
        let posted = store.create_invoice(company, &draft).unwrap();
        let invoice = store.invoices(company).unwrap().remove(0);
        assert_eq!(
            (invoice.id, invoice.total, invoice.outstanding),
            (posted, 10500, 10500)
        );
        assert_eq!(invoice.lines[0].price_label(), "105.00 / Case");
        assert_eq!(
            invoice.lines[0].quantity_label(),
            "25.000 pcs stock · 24.000 billed"
        );
        assert_eq!(
            store
                .stock_valuation_at_average_cost(company, "2026-05-01")
                .unwrap()
                .known_value,
            10500
        );
    }
}
