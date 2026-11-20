//! Standard cost schedules and management reports for stocks and item sales.
use crate::{inventory::StockItem, parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::{BTreeMap, HashMap};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE standard_stock_costs(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            date TEXT NOT NULL,
            unit_cost INTEGER NOT NULL CHECK(unit_cost >= 0),
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX standard_stock_costs_item ON standard_stock_costs(company_id, item_id, date, id);",
    )?;
    crate::periods::protect_dated_table(conn, "standard_stock_costs")
}

#[derive(Debug, Clone)]
pub struct StandardCostRate {
    pub date: String,
    pub unit_cost: i64,
    pub reason: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone)]
pub struct StockValuationRow {
    pub item: StockItem,
    pub quantity: i128,
    pub unit_cost: Option<i64>,
    pub value: Option<i128>,
}

#[derive(Debug, Clone)]
pub struct StockValuation {
    pub date: NaiveDate,
    pub rows: Vec<StockValuationRow>,
    pub known_value: i128,
    pub missing_rates: usize,
}

#[derive(Debug, Clone)]
pub struct SalesCostRow {
    pub invoice_id: i64,
    pub voucher_id: i64,
    pub date: NaiveDate,
    pub reference: String,
    pub document: crate::stock_sales::SalesStockDocument,
    pub reversal: bool,
    pub item: String,
    pub unit: String,
    pub quantity: i128,
    pub net_sales: i128,
    pub unit_cost: Option<i64>,
    pub standard_cost: Option<i128>,
}
impl SalesCostRow {
    pub fn margin(&self) -> Option<i128> {
        self.standard_cost.map(|cost| self.net_sales - cost)
    }
}

#[derive(Debug, Clone)]
pub struct SalesCostReport {
    pub rows: Vec<SalesCostRow>,
    pub net_sales: i128,
    pub costed_net_sales: i128,
    pub standard_cost: i128,
    pub costed_margin: i128,
    pub missing_rates: usize,
}

type Schedule = HashMap<i64, BTreeMap<NaiveDate, i64>>;

fn rates(conn: &Connection, company: i64) -> Result<Schedule> {
    let mut schedule = Schedule::new();
    let mut stmt = conn.prepare(
        "SELECT item_id, date, unit_cost FROM standard_stock_costs WHERE company_id = ?1 ORDER BY id",
    )?;
    let mut rows = stmt.query([company])?;
    while let Some(row) = rows.next()? {
        let item: i64 = row.get(0)?;
        let date = parse_date(&row.get::<_, String>(1)?)?;
        let rate = row.get(2)?;
        schedule.entry(item).or_default().insert(date, rate);
    }
    Ok(schedule)
}

fn effective(schedule: &Schedule, item: i64, date: NaiveDate) -> Option<i64> {
    schedule
        .get(&item)?
        .range(..=date)
        .next_back()
        .map(|(_, &cost)| cost)
}

fn add(total: i128, amount: i128) -> Result<i128> {
    total
        .checked_add(amount)
        .ok_or_else(|| Error::Validation("Standard cost report amount is too large".into()))
}

fn priced(quantity: i128, unit_cost: i64) -> Result<i128> {
    let cost = i128::from(unit_cost);
    // Quantities are in thousandths. Round halves away from zero so a note
    // always offsets the cost of its original sales line exactly.
    let fraction = (quantity % 1000) * cost;
    let rounded = ((fraction.abs() + 500) / 1000) * fraction.signum();
    let whole = (quantity / 1000)
        .checked_mul(cost)
        .ok_or_else(|| Error::Validation("Standard stock value is too large".into()))?;
    add(whole, rounded)
}

impl Store {
    pub fn record_standard_stock_cost(
        &mut self,
        company: i64,
        item: i64,
        date: &str,
        unit_cost: i64,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let reason = required(reason, "Reason for setting the cost")?;
        if unit_cost < 0 {
            return Err(Error::Validation("Standard cost cannot be negative".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT INTO standard_stock_costs(company_id, item_id, date, unit_cost, reason)
             SELECT ?1, id, ?3, ?4, ?5 FROM stock_items WHERE company_id = ?1 AND id = ?2",
            params![company, item, date, unit_cost, reason],
        )?;
        if inserted != 1 {
            return Err(Error::Validation(
                "Stock item not found in this company".into(),
            ));
        }
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn standard_stock_cost_history(
        &self,
        company: i64,
        item: i64,
    ) -> Result<Vec<StandardCostRate>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, unit_cost, reason, recorded_at FROM standard_stock_costs
             WHERE company_id = ?1 AND item_id = ?2 ORDER BY id DESC",
        )?;
        let rows = stmt.query_map(params![company, item], |row| {
            Ok(StandardCostRate {
                date: row.get(0)?,
                unit_cost: row.get(1)?,
                reason: row.get(2)?,
                recorded_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Value all locations together at the standard rates effective on this date.
    pub fn stock_valuation_at_standard_cost(
        &self,
        company: i64,
        date: &str,
    ) -> Result<StockValuation> {
        let date = parse_date(date)?;
        let tx = self.conn.unchecked_transaction()?;
        let schedule = rates(&tx, company)?;
        let balances = self.stock_balances(company, Some(&date.format("%Y-%m-%d").to_string()))?;
        let mut report = StockValuation {
            date,
            rows: Vec::with_capacity(balances.len()),
            known_value: 0,
            missing_rates: 0,
        };
        for balance in balances {
            let unit_cost = effective(&schedule, balance.item.id, date);
            let value = if balance.quantity == 0 {
                Some(0)
            } else {
                unit_cost
                    .map(|cost| priced(balance.quantity, cost))
                    .transpose()?
            };
            if let Some(value) = value {
                report.known_value = add(report.known_value, value)?;
            } else {
                report.missing_rates += 1;
            }
            report.rows.push(StockValuationRow {
                item: balance.item,
                quantity: balance.quantity,
                unit_cost,
                value,
            });
        }
        tx.commit()?;
        Ok(report)
    }

    /// Use the original invoice date for the standard rate on both a sale and its reversal.
    pub fn sales_at_standard_cost(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<SalesCostReport> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(start), Some(end)) if start > end) {
            return Err(Error::Validation(
                "Report start date must not follow the end date".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let schedule = rates(&tx, company)?;
        let mut report = SalesCostReport {
            rows: Vec::new(),
            net_sales: 0,
            costed_net_sales: 0,
            standard_cost: 0,
            costed_margin: 0,
            missing_rates: 0,
        };
        for sale in crate::stock_sales::stock_sales(&tx, company, start, end)? {
            let quantity_issued = sale.issue_quantity();
            let unit_cost =
                effective(&schedule, sale.item_id, sale.cost_date).filter(|_| quantity_issued > 0);
            let standard_cost = unit_cost
                .map(|cost| priced(quantity_issued, cost))
                .transpose()?
                .map(|total| sale.allocated_issue_cost(total));
            report.net_sales = add(report.net_sales, sale.net_sales)?;
            if let Some(cost) = standard_cost {
                report.costed_net_sales = add(report.costed_net_sales, sale.net_sales)?;
                report.standard_cost = add(report.standard_cost, cost)?;
                report.costed_margin = add(report.costed_margin, sale.net_sales - cost)?;
            } else {
                report.missing_rates += 1;
            }
            report.rows.push(SalesCostRow {
                invoice_id: sale.invoice_id,
                voucher_id: sale.voucher_id,
                date: sale.date,
                reference: sale.reference,
                document: sale.document,
                reversal: sale.reversal,
                item: sale.item,
                unit: sale.unit,
                quantity: sale.quantity,
                net_sales: sale.net_sales,
                unit_cost,
                standard_cost,
            });
        }
        tx.commit()?;
        Ok(report)
    }
}
