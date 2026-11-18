//! Company-wide inventory costs for receipts and issues.
use crate::inventory::{InvoiceKind, StockItem};
use crate::stock_sales::{stock_sales, SaleCostOrigin, StockSale};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{HashMap, HashSet, VecDeque};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE stock_receipt_costs(
            id INTEGER PRIMARY KEY,
            adjustment_id INTEGER NOT NULL REFERENCES stock_adjustments(id),
            amount INTEGER NOT NULL CHECK(amount >= 0),
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX stock_receipt_costs_adjustment ON stock_receipt_costs(adjustment_id, id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct StockReceipt {
    pub id: i64,
    pub date: String,
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub quantity: i64,
    pub description: String,
    pub cost: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ReceiptCostEntry {
    pub amount: i64,
    pub reason: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone)]
pub enum ActualCostIssue {
    MissingReceiptCost { date: NaiveDate, receipt_id: i64 },
    InsufficientStock { date: NaiveDate, source: String },
}
impl std::fmt::Display for ActualCostIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingReceiptCost { date, receipt_id } => {
                write!(
                    f,
                    "Receipt {receipt_id} on {date} needs an acquisition cost"
                )
            }
            Self::InsufficientStock { date, source } => {
                write!(f, "{source} on {date} exceeds the available quantity")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ActualCostMethod {
    #[default]
    WeightedAverage,
    Fifo,
}
impl ActualCostMethod {
    pub const ALL: [Self; 2] = [Self::WeightedAverage, Self::Fifo];

    pub fn label(self) -> &'static str {
        match self {
            Self::WeightedAverage => "Weighted average",
            Self::Fifo => "FIFO",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActualCostRow {
    pub item: StockItem,
    pub quantity: i128,
    pub value: Option<i128>,
    pub issue: Option<ActualCostIssue>,
}

#[derive(Debug, Clone)]
pub struct ActualCostValuation {
    pub date: NaiveDate,
    pub method: ActualCostMethod,
    pub rows: Vec<ActualCostRow>,
    pub known_value: i128,
    pub unavailable: usize,
}

#[derive(Debug, Clone)]
pub struct ActualSalesCostRow {
    pub sale: StockSale,
    pub cost: Option<i128>,
    pub issue: Option<ActualCostIssue>,
}
impl ActualSalesCostRow {
    pub fn margin(&self) -> Option<i128> {
        self.cost
            .and_then(|cost| self.sale.net_sales.checked_sub(cost))
    }
}

#[derive(Debug, Clone)]
pub struct ActualSalesCostReport {
    pub method: ActualCostMethod,
    pub rows: Vec<ActualSalesCostRow>,
    pub net_sales: i128,
    pub costed_net_sales: i128,
    pub cost_of_sales: i128,
    pub costed_margin: i128,
    pub unavailable_lines: usize,
}

pub type AverageCostRow = ActualCostRow;
pub type AverageCostValuation = ActualCostValuation;

#[derive(Debug, Clone, Copy)]
enum Movement {
    Receipt(Option<i64>),
    Purchase(i64),
    Issue {
        origin: SaleCostOrigin,
        reversed: bool,
    },
    IssueReversal(SaleCostOrigin),
    PurchaseReversal,
    AdjustmentOut,
}
impl Movement {
    fn priority(self) -> u8 {
        match self {
            Self::Receipt(_) => 0,
            Self::Purchase(_) => 1,
            Self::IssueReversal(_) => 2,
            Self::Issue { .. } => 3,
            Self::PurchaseReversal => 4,
            Self::AdjustmentOut => 5,
        }
    }
}

struct Event {
    date: NaiveDate,
    source_id: i64,
    quantity: i128,
    movement: Movement,
}

#[derive(Default, Clone, Copy)]
struct Layer {
    quantity: i128,
    value: i128,
}
impl Layer {
    /// Release a proportional share of the remaining value in this layer.
    fn take(&mut self, quantity: i128) -> Result<Self> {
        let value = if quantity == self.quantity {
            self.value
        } else {
            let numerator = self
                .value
                .checked_mul(quantity)
                .ok_or_else(amount_too_large)?;
            let quotient = numerator / self.quantity;
            let remainder = numerator % self.quantity;
            quotient + i128::from(remainder >= self.quantity - remainder)
        };
        self.quantity -= quantity;
        self.value -= value;
        Ok(Self { quantity, value })
    }
}

#[derive(Default)]
struct Pool {
    available: Layer,
    // Weighted average has a single combined layer, represented by None.
    fifo_layers: Option<VecDeque<Layer>>,
    sale_costs: HashMap<SaleCostOrigin, Vec<Layer>>,
}

fn amount_too_large() -> Error {
    Error::Validation("Inventory cost report amount is too large".into())
}

impl Pool {
    fn receive(&mut self, layer: Layer) -> Result<()> {
        self.available.quantity = self
            .available
            .quantity
            .checked_add(layer.quantity)
            .ok_or_else(amount_too_large)?;
        self.available.value = self
            .available
            .value
            .checked_add(layer.value)
            .ok_or_else(amount_too_large)?;
        if let Some(layers) = &mut self.fifo_layers {
            layers.push_back(layer);
        }
        Ok(())
    }

    fn issue(&mut self, quantity: i128) -> Result<Vec<Layer>> {
        let Some(layers) = &mut self.fifo_layers else {
            return Ok(vec![self.available.take(quantity)?]);
        };
        let mut unfilled = quantity;
        let mut portions = Vec::new();
        while unfilled > 0 {
            let layer = layers.front_mut().ok_or_else(|| {
                Error::Validation("FIFO layers do not cover the available units".into())
            })?;
            let take = unfilled.min(layer.quantity);
            let portion = layer.take(take)?;
            self.available.quantity -= portion.quantity;
            self.available.value -= portion.value;
            unfilled -= take;
            portions.push(portion);
            if layer.quantity == 0 {
                layers.pop_front();
            }
        }
        Ok(portions)
    }
}

fn calculate(
    events: &mut [Event],
    method: ActualCostMethod,
    mut reported_sale_costs: Option<&mut HashMap<SaleCostOrigin, i128>>,
) -> Result<std::result::Result<Pool, ActualCostIssue>> {
    events.sort_by_key(|event| (event.date, event.movement.priority(), event.source_id));
    let mut pool = Pool {
        fifo_layers: (method == ActualCostMethod::Fifo).then(VecDeque::new),
        ..Pool::default()
    };
    for event in events {
        match event.movement {
            Movement::Receipt(Some(value)) | Movement::Purchase(value) => {
                pool.receive(Layer {
                    quantity: event.quantity,
                    value: i128::from(value),
                })?;
            }
            Movement::Receipt(None) => {
                return Ok(Err(ActualCostIssue::MissingReceiptCost {
                    date: event.date,
                    receipt_id: event.source_id,
                }));
            }
            Movement::IssueReversal(origin) => {
                let restored = pool.sale_costs.remove(&origin).ok_or_else(|| {
                    Error::Validation("Original stock issue cost is missing for a reversal".into())
                })?;
                for layer in restored {
                    pool.receive(layer)?;
                }
            }
            issue => {
                if event.quantity > pool.available.quantity {
                    let source = match issue {
                        Movement::Issue {
                            origin: SaleCostOrigin::Invoice(line),
                            ..
                        } => format!("Sale line {line}"),
                        Movement::Issue {
                            origin: SaleCostOrigin::Delivery(line),
                            ..
                        } => format!("Delivery note line {line}"),
                        Movement::PurchaseReversal => {
                            format!("Purchase reversal for line {}", event.source_id)
                        }
                        Movement::AdjustmentOut => format!("Stock adjustment {}", event.source_id),
                        _ => unreachable!("only stock issue events reach this branch"),
                    };
                    return Ok(Err(ActualCostIssue::InsufficientStock {
                        date: event.date,
                        source,
                    }));
                }
                let portions = pool.issue(event.quantity)?;
                if let Movement::Issue { origin, reversed } = issue {
                    if let Some(reported_sale_costs) = reported_sale_costs.as_deref_mut() {
                        reported_sale_costs
                            .insert(origin, portions.iter().map(|layer| layer.value).sum());
                    }
                    if reversed {
                        pool.sale_costs.insert(origin, portions);
                    }
                }
            }
        }
    }
    Ok(Ok(pool))
}

fn events(conn: &Connection, company: i64, end: NaiveDate) -> Result<HashMap<i64, Vec<Event>>> {
    let mut events = HashMap::<i64, Vec<Event>>::new();
    {
        let mut stmt = conn.prepare(
            "SELECT a.id, a.item_id, a.date, a.quantity, (
                SELECT c.amount FROM stock_receipt_costs c WHERE c.adjustment_id = a.id ORDER BY c.id DESC LIMIT 1
             ), l.id, n.reversed_on, l.reversal_adjustment_id
             FROM stock_adjustments a
             LEFT JOIN stock_note_lines l ON l.adjustment_id = a.id
             LEFT JOIN stock_notes n ON n.id = l.note_id AND n.company_id = a.company_id
             WHERE a.company_id = ?1
             AND NOT EXISTS(SELECT 1 FROM invoice_reversal_stock r WHERE r.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_note_lines r WHERE r.reversal_adjustment_id = a.id)",
        )?;
        let mut rows = stmt.query([company])?;
        while let Some(row) = rows.next()? {
            let date = parse_date(&row.get::<_, String>(2)?)?;
            if date > end {
                continue;
            }
            let quantity = i128::from(row.get::<_, i64>(3)?);
            let origin = row.get::<_, Option<i64>>(5)?.map(SaleCostOrigin::Delivery);
            let reversal = row
                .get::<_, Option<String>>(6)?
                .as_deref()
                .map(parse_date)
                .transpose()?;
            if reversal == Some(date) {
                continue;
            }
            let reversal = reversal.filter(|day| *day <= end);
            let movement = if quantity > 0 {
                Movement::Receipt(row.get(4)?)
            } else if let Some(origin) = origin {
                Movement::Issue {
                    origin,
                    reversed: reversal.is_some(),
                }
            } else {
                Movement::AdjustmentOut
            };
            let item_events = events.entry(row.get(1)?).or_default();
            item_events.push(Event {
                date,
                source_id: row.get(0)?,
                quantity: quantity.abs(),
                movement,
            });
            if let Some(date) = reversal {
                let movement = if quantity > 0 {
                    Movement::AdjustmentOut
                } else {
                    Movement::IssueReversal(origin.ok_or_else(|| {
                        Error::Validation(
                            "Reversed delivery is missing its original movement".into(),
                        )
                    })?)
                };
                item_events.push(Event {
                    date,
                    source_id: row.get(7)?,
                    quantity: quantity.abs(),
                    movement,
                });
            }
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT l.id, l.item_id, i.kind, v.date, l.quantity, l.amount, rv.date
             FROM physical_invoice_lines l JOIN invoices i ON i.id = l.invoice_id
             JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
             LEFT JOIN invoice_reversals r ON r.invoice_id = i.id AND r.company_id = i.company_id
             LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
             WHERE i.company_id = ?1",
        )?;
        let mut rows = stmt.query([company])?;
        while let Some(row) = rows.next()? {
            let date = parse_date(&row.get::<_, String>(3)?)?;
            if date > end {
                continue;
            }
            let reversal = row
                .get::<_, Option<String>>(6)?
                .as_deref()
                .map(parse_date)
                .transpose()?;
            // No stock remains affected by an invoice reversed on the same day.
            if reversal == Some(date) {
                continue;
            }
            let kind = InvoiceKind::parse(&row.get::<_, String>(2)?)?;
            let quantity = i128::from(row.get::<_, i64>(4)?);
            let line: i64 = row.get(0)?;
            let item_events = events.entry(row.get(1)?).or_default();
            item_events.push(Event {
                date,
                source_id: line,
                quantity,
                movement: match kind {
                    InvoiceKind::Sale => Movement::Issue {
                        origin: SaleCostOrigin::Invoice(line),
                        reversed: reversal.is_some_and(|date| date <= end),
                    },
                    InvoiceKind::Purchase => Movement::Purchase(row.get(5)?),
                },
            });
            if let Some(date) = reversal.filter(|date| *date <= end) {
                item_events.push(Event {
                    date,
                    source_id: line,
                    quantity,
                    movement: match kind {
                        InvoiceKind::Sale => Movement::IssueReversal(SaleCostOrigin::Invoice(line)),
                        InvoiceKind::Purchase => Movement::PurchaseReversal,
                    },
                });
            }
        }
    }
    Ok(events)
}

pub(crate) fn insert_receipt_cost(
    conn: &Connection,
    receipt: i64,
    amount: i64,
    reason: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO stock_receipt_costs(adjustment_id, amount, reason) VALUES (?1, ?2, ?3)",
        params![receipt, amount, reason],
    )?;
    Ok(())
}

impl Store {
    /// List up to 100 quantity increases, optionally finding one by its adjustment ID.
    pub fn stock_receipts(
        &self,
        company: i64,
        adjustment: Option<i64>,
    ) -> Result<Vec<StockReceipt>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.date, i.name, i.unit, l.name, b.name, a.quantity, a.reason, (
                SELECT c.amount FROM stock_receipt_costs c WHERE c.adjustment_id = a.id ORDER BY id DESC LIMIT 1
             )
             FROM stock_adjustments a JOIN stock_items i ON i.id = a.item_id AND i.company_id = a.company_id
             JOIN stock_locations l ON l.id = a.location_id AND l.company_id = a.company_id
             LEFT JOIN stock_batches b ON b.id = a.batch_id
             WHERE a.company_id = ?1 AND a.quantity > 0 AND (?2 IS NULL OR a.id = ?2)
             AND NOT EXISTS(SELECT 1 FROM invoice_reversal_stock r WHERE r.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_note_lines r WHERE r.reversal_adjustment_id = a.id)
             ORDER BY a.date DESC, a.id DESC LIMIT 100",
        )?;
        let rows = stmt.query_map(params![company, adjustment], |row| {
            Ok(StockReceipt {
                id: row.get(0)?,
                date: row.get(1)?,
                item: row.get(2)?,
                unit: row.get(3)?,
                location: row.get(4)?,
                batch: row.get(5)?,
                quantity: row.get(6)?,
                description: row.get(7)?,
                cost: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_stock_receipt_cost(
        &mut self,
        company: i64,
        receipt: i64,
        amount: i64,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason for setting the acquisition cost")?;
        if amount < 0 {
            return Err(Error::Validation(
                "Acquisition cost cannot be negative".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let date: String = tx.query_row(
            "SELECT a.date FROM stock_adjustments a WHERE a.company_id = ?1 AND a.id = ?2 AND a.quantity > 0
             AND NOT EXISTS(SELECT 1 FROM invoice_reversal_stock r WHERE r.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_note_lines r WHERE r.reversal_adjustment_id = a.id)",
             params![company, receipt], |row| row.get(0),
        ).optional()?.ok_or_else(|| Error::Validation("Stock receipt adjustment not found in this company".into()))?;
        let date = parse_date(&date)?;
        if crate::periods::cutoff(&tx, company)?.is_some_and(|cutoff| date <= cutoff) {
            return Err(Error::Validation(
                crate::periods::LOCKED_PERIOD_MESSAGE.into(),
            ));
        }
        let previous: Option<i64> = tx.query_row(
            "SELECT amount FROM stock_receipt_costs WHERE adjustment_id = ?1 ORDER BY id DESC LIMIT 1",
            [receipt], |row| row.get(0),
        ).optional()?;
        if previous == Some(amount) {
            return Err(Error::Validation(
                "This acquisition cost is already saved".into(),
            ));
        }
        insert_receipt_cost(&tx, receipt, amount, reason)?;
        tx.commit()?;
        Ok(())
    }

    pub fn stock_receipt_cost_history(
        &self,
        company: i64,
        receipt: i64,
    ) -> Result<Vec<ReceiptCostEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.amount, c.reason, c.recorded_at FROM stock_receipt_costs c
             JOIN stock_adjustments a ON a.id = c.adjustment_id
             WHERE a.company_id = ?1 AND a.id = ?2 ORDER BY c.id DESC",
        )?;
        let rows = stmt.query_map(params![company, receipt], |row| {
            Ok(ReceiptCostEntry {
                amount: row.get(0)?,
                reason: row.get(1)?,
                recorded_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Report weighted average value for all company locations together.
    pub fn stock_valuation_at_average_cost(
        &self,
        company: i64,
        date: &str,
    ) -> Result<AverageCostValuation> {
        self.stock_valuation_at_actual_cost(company, date, ActualCostMethod::WeightedAverage)
    }

    pub fn stock_valuation_at_actual_cost(
        &self,
        company: i64,
        date: &str,
        method: ActualCostMethod,
    ) -> Result<ActualCostValuation> {
        let end = parse_date(date)?;
        let tx = self.conn.unchecked_transaction()?;
        let mut events = events(&tx, company, end)?;
        let balances = self.stock_balances(company, Some(date))?;
        let mut report = ActualCostValuation {
            date: end,
            method,
            rows: Vec::with_capacity(balances.len()),
            known_value: 0,
            unavailable: 0,
        };
        for balance in balances {
            let (value, issue) = match calculate(
                &mut events.remove(&balance.item.id).unwrap_or_default(),
                method,
                None,
            )? {
                Ok(pool) => {
                    if pool.available.quantity != balance.quantity {
                        return Err(Error::Validation(format!(
                            "Stock movements do not reconcile for {}",
                            balance.item.name
                        )));
                    }
                    report.known_value = report
                        .known_value
                        .checked_add(pool.available.value)
                        .ok_or_else(amount_too_large)?;
                    (Some(pool.available.value), None)
                }
                Err(issue) => {
                    report.unavailable += 1;
                    (None, Some(issue))
                }
            };
            report.rows.push(ActualCostRow {
                item: balance.item,
                quantity: balance.quantity,
                value,
                issue,
            });
        }
        tx.commit()?;
        Ok(report)
    }

    /// Cost item sales and reversals with the same valuation rules as the stock report.
    pub fn sales_at_actual_cost(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        method: ActualCostMethod,
    ) -> Result<ActualSalesCostReport> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(start), Some(end)) if start > end) {
            return Err(Error::Validation(
                "Report start date must not follow the end date".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut sales = stock_sales(&tx, company, start, end)?;
        sales.retain(|sale| !sale.reversed_same_day);
        let mut report = ActualSalesCostReport {
            method,
            rows: Vec::with_capacity(sales.len()),
            net_sales: 0,
            costed_net_sales: 0,
            cost_of_sales: 0,
            costed_margin: 0,
            unavailable_lines: 0,
        };
        if sales.is_empty() {
            tx.commit()?;
            return Ok(report);
        }
        let mut events = events(&tx, company, end.unwrap_or(NaiveDate::MAX))?;
        let items = sales
            .iter()
            .map(|sale| sale.item_id)
            .collect::<HashSet<_>>();
        let mut issues = HashMap::new();
        let mut costs = HashMap::new();
        for item in items {
            if let Err(issue) = calculate(
                &mut events.remove(&item).unwrap_or_default(),
                method,
                Some(&mut costs),
            )? {
                issues.insert(item, issue);
            }
        }
        for sale in sales {
            report.net_sales = report
                .net_sales
                .checked_add(sale.net_sales)
                .ok_or_else(amount_too_large)?;
            let cost = costs
                .get(&sale.cost_origin)
                .map(|&amount| sale.allocated_issue_cost(amount));
            let mut issue = None;
            if let Some(cost) = cost {
                report.costed_net_sales = report
                    .costed_net_sales
                    .checked_add(sale.net_sales)
                    .ok_or_else(amount_too_large)?;
                report.cost_of_sales = report
                    .cost_of_sales
                    .checked_add(cost)
                    .ok_or_else(amount_too_large)?;
                let margin = sale
                    .net_sales
                    .checked_sub(cost)
                    .ok_or_else(amount_too_large)?;
                report.costed_margin = report
                    .costed_margin
                    .checked_add(margin)
                    .ok_or_else(amount_too_large)?;
            } else {
                let problem = issues.get(&sale.item_id).ok_or_else(|| {
                    Error::Validation(format!(
                        "Sale line {} has no matching cost event",
                        sale.line_id
                    ))
                })?;
                issue = Some(problem.clone());
                report.unavailable_lines += 1;
            }
            report.rows.push(ActualSalesCostRow { sale, cost, issue });
        }
        tx.commit()?;
        Ok(report)
    }
}
