//! Company-wide inventory costs for receipts and issues.
use crate::inventory::{InvoiceKind, StockItem};
use crate::stock_sales::{stock_sales, SaleCostOrigin, StockSale};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

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
    MissingJournalCost { date: NaiveDate, journal_id: i64 },
    InsufficientStock { date: NaiveDate, source: String },
    PendingStockIssue,
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
            Self::MissingJournalCost { date, journal_id } => {
                write!(
                    f,
                    "Stock journal {journal_id} on {date} needs an acquisition cost"
                )
            }
            Self::InsufficientStock { date, source } => {
                write!(f, "{source} on {date} exceeds the available quantity")
            }
            Self::PendingStockIssue => {
                write!(f, "Billed quantities are still awaiting stock issue")
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

pub const DEFAULT_AGEING_CUTOFFS: [u32; 3] = [30, 60, 90];

#[derive(Debug, Clone)]
pub struct StockAgeingReport {
    pub date: NaiveDate,
    pub cutoffs: [u32; 3],
    pub rows: Vec<StockAgeingRow>,
}

impl StockAgeingReport {
    pub fn band_labels(&self) -> [String; 4] {
        let [a, b, c] = self.cutoffs.map(u64::from);
        [
            format!("0–{a} days"),
            format!("{}–{b} days", a + 1),
            format!("{}–{c} days", b + 1),
            format!("{}+ days", c + 1),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct StockAgeingRow {
    pub item: StockItem,
    pub quantity: i128,
    pub breakdown: Option<StockAgeingBreakdown>,
    pub issue: Option<ActualCostIssue>,
}

#[derive(Debug, Clone)]
pub struct StockAgeingBreakdown {
    pub bands: [AgeingAmount; 4],
    pub receipts: Vec<StockAgeingReceipt>,
    pub fifo_value: Option<i128>,
}

#[derive(Debug, Clone, Copy)]
pub struct AgeingAmount {
    pub quantity: i128,
    pub fifo_value: Option<i128>,
}

#[derive(Debug, Clone)]
pub struct StockAgeingReceipt {
    pub received_on: NaiveDate,
    pub age_days: i64,
    pub amount: AgeingAmount,
}

#[cfg(test)]
mod ageing_tests {
    use super::*;
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::stock_returns::StockReturnLineDraft;
    use crate::Category;

    #[test]
    fn ageing_reconciles_fifo_quantities_and_values_at_the_requested_date() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let party = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let opening = store
            .adjust_stock_batch(company, item, location, None, "2026-01-01", 5000, "Opening")
            .unwrap();
        store
            .set_stock_receipt_cost(company, opening, 5000, "Opening value")
            .unwrap();
        let second = store
            .adjust_stock_batch(company, item, location, None, "2026-01-31", 4000, "Receipt")
            .unwrap();
        let broken = store
            .create_stock_item(company, "Out of sequence", "Each")
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                broken,
                location,
                None,
                "2026-01-02",
                -1000,
                "Before receipt",
            )
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                broken,
                location,
                None,
                "2026-02-01",
                2000,
                "Receipt",
            )
            .unwrap();
        let note = store
            .post_stock_note(
                company,
                &StockNoteDraft {
                    kind: InvoiceKind::Sale,
                    date: "2026-02-01".into(),
                    reference: "DEL-01".into(),
                    party_ledger_id: party,
                    order_id: None,
                    narration: String::new(),
                    lines: vec![StockNoteLineDraft {
                        item_id: item,
                        location_id: location,
                        batch_id: None,
                        quantity: 6000,
                        unit_price: 2500,
                    }],
                },
            )
            .unwrap();
        let note_line = store.stock_note_lines(company, note).unwrap()[0].id;
        store
            .post_stock_return(
                company,
                note,
                "2026-02-20",
                "RET-01",
                "Returned to inventory",
                &[StockReturnLineDraft {
                    note_line,
                    quantity: 2000,
                }],
            )
            .unwrap();
        assert!(store
            .stock_ageing(company, "2026-02-21", [30, 20, 90])
            .is_err());
        let report = store
            .stock_ageing(company, "2026-01-31", [0, 10, 30])
            .unwrap();
        let row = report.rows.iter().find(|row| row.item.id == item).unwrap();
        assert_eq!(
            row.breakdown
                .as_ref()
                .unwrap()
                .bands
                .map(|band| band.quantity),
            [4000, 0, 5000, 0]
        );

        let cutoffs = [10, 20, 30];
        let report = store.stock_ageing(company, "2026-02-15", cutoffs).unwrap();
        let row = report.rows.iter().find(|row| row.item.id == item).unwrap();
        let ageing = row.breakdown.as_ref().unwrap();
        assert_eq!(row.quantity, 3000);
        assert_eq!(ageing.bands.map(|band| band.quantity), [0, 3000, 0, 0]);
        assert_eq!((ageing.receipts[0].age_days, ageing.fifo_value), (15, None));
        assert!(
            matches!(row.issue, Some(ActualCostIssue::MissingReceiptCost { receipt_id, .. }) if receipt_id == second)
        );
        let negative = report
            .rows
            .iter()
            .find(|row| row.item.id == broken)
            .unwrap();
        assert_eq!(negative.quantity, 1000);
        assert!(negative.breakdown.is_none());
        assert!(matches!(
            negative.issue,
            Some(ActualCostIssue::InsufficientStock { .. })
        ));

        store
            .set_stock_receipt_cost(company, second, 8000, "Acquisition value")
            .unwrap();
        let end = "2026-02-21";
        let report = store.stock_ageing(company, end, cutoffs).unwrap();
        let row = report.rows.iter().find(|row| row.item.id == item).unwrap();
        let ageing = row.breakdown.as_ref().unwrap();
        assert_eq!(row.quantity, 5000);
        assert_eq!(ageing.bands.map(|band| band.quantity), [2000, 0, 3000, 0]);
        assert_eq!(
            (
                ageing.receipts[1].received_on.to_string().as_str(),
                ageing.receipts[1].age_days
            ),
            ("2026-02-20", 1)
        );
        assert_eq!(ageing.fifo_value, Some(8333));
        let fifo = store
            .stock_valuation_at_actual_cost(company, end, ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(fifo.known_value, ageing.fifo_value.unwrap());
        assert_eq!(
            ageing
                .bands
                .iter()
                .map(|band| band.fifo_value.unwrap())
                .sum::<i128>(),
            fifo.known_value
        );

        let path =
            std::env::temp_dir().join(format!("balance-ageing-test-{}.csv", std::process::id()));
        store
            .export_stock_ageing_csv(company, end, cutoffs, &path)
            .unwrap();
        let data = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let mut csv = csv::Reader::from_reader(data.as_bytes());
        let headers = csv.headers().unwrap().clone();
        let record_type = headers
            .iter()
            .position(|column| column == "row_type")
            .unwrap();
        let rows = csv
            .records()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| &row[record_type] == "receipt")
                .count(),
            2
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IssueOrigin {
    Sale(SaleCostOrigin),
    SupplierReturn(i64),
}

#[derive(Debug)]
enum Movement {
    Receipt(Option<i64>),
    JournalReceipt {
        journal_id: i64,
        amount: Option<i128>,
    },
    Purchase(i64),
    Issue {
        origin: IssueOrigin,
        retain_cost: bool,
    },
    IssueReversal(IssueOrigin),
    SaleReturn {
        origin: IssueOrigin,
        same_day_adjustment: Option<i64>,
        share: crate::stock_notes::InvoiceQuantityShare,
    },
    PurchaseReversal,
    AdjustmentOut,
}
struct Event {
    date: NaiveDate,
    source_id: i64,
    quantity: i128,
    movement: Movement,
}

impl Event {
    fn order(&self) -> (NaiveDate, u8, i64, bool, i64) {
        let priority = match &self.movement {
            Movement::Receipt(_) | Movement::JournalReceipt { .. } => 0,
            Movement::Purchase(_) => 1,
            Movement::SaleReturn {
                same_day_adjustment: Some(source),
                ..
            } => {
                // On the delivery date, restore the units after costing their issue.
                return (self.date, 3, *source, true, self.source_id);
            }
            Movement::IssueReversal(_) | Movement::SaleReturn { .. } => 2,
            Movement::Issue {
                origin: IssueOrigin::Sale(SaleCostOrigin::ReturnCounter(_)),
                ..
            } => 5,
            Movement::Issue {
                origin: IssueOrigin::Sale(_),
                ..
            } => 3,
            Movement::PurchaseReversal
            | Movement::Issue {
                origin: IssueOrigin::SupplierReturn(_),
                ..
            } => 4,
            Movement::AdjustmentOut => 5,
        };
        (self.date, priority, self.source_id, false, 0)
    }
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
    fifo_layers: Option<VecDeque<(NaiveDate, Layer)>>,
    issue_layers: HashMap<IssueOrigin, Vec<Layer>>,
}

#[derive(Clone, Copy)]
enum Calculation {
    Valuation(ActualCostMethod),
    AgeingQuantities,
}

fn amount_too_large() -> Error {
    Error::Validation("Inventory cost report amount is too large".into())
}

impl Pool {
    fn receive(&mut self, date: NaiveDate, layer: Layer) -> Result<()> {
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
            layers.push_back((date, layer));
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
            let (_, layer) = layers.front_mut().ok_or_else(|| {
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
    calculation: Calculation,
    mut reported_sale_costs: Option<&mut HashMap<SaleCostOrigin, i128>>,
) -> Result<std::result::Result<Pool, ActualCostIssue>> {
    events.sort_by_key(Event::order);
    let track_cost = matches!(calculation, Calculation::Valuation(_));
    let mut pool = Pool {
        fifo_layers: (!matches!(
            calculation,
            Calculation::Valuation(ActualCostMethod::WeightedAverage)
        ))
        .then(VecDeque::new),
        ..Pool::default()
    };
    for event in events {
        match &event.movement {
            Movement::Receipt(_) | Movement::JournalReceipt { .. } | Movement::Purchase(_) => {
                let value = match (&event.movement, track_cost) {
                    (_, false) => 0,
                    (Movement::Receipt(Some(value)) | Movement::Purchase(value), true) => {
                        i128::from(*value)
                    }
                    (
                        Movement::JournalReceipt {
                            amount: Some(value),
                            ..
                        },
                        true,
                    ) => *value,
                    (
                        Movement::JournalReceipt {
                            journal_id,
                            amount: None,
                        },
                        true,
                    ) => {
                        return Ok(Err(ActualCostIssue::MissingJournalCost {
                            date: event.date,
                            journal_id: *journal_id,
                        }))
                    }
                    _ => {
                        return Ok(Err(ActualCostIssue::MissingReceiptCost {
                            date: event.date,
                            receipt_id: event.source_id,
                        }))
                    }
                };
                pool.receive(
                    event.date,
                    Layer {
                        quantity: event.quantity,
                        value,
                    },
                )?;
            }
            Movement::IssueReversal(origin) => {
                let restored = pool.issue_layers.remove(origin).ok_or_else(|| {
                    Error::Validation("Original stock issue cost is missing for a reversal".into())
                })?;
                for layer in restored {
                    pool.receive(event.date, layer)?;
                }
            }
            Movement::SaleReturn { origin, share, .. } => {
                let issued = pool.issue_layers.get(origin).ok_or_else(|| {
                    Error::Validation("Original delivery cost is missing for a return".into())
                })?;
                pool.receive(
                    event.date,
                    Layer {
                        quantity: event.quantity,
                        value: share.apportion(issued.iter().map(|layer| layer.value).sum()),
                    },
                )?;
            }
            issue => {
                if event.quantity > pool.available.quantity {
                    let source = match issue {
                        Movement::Issue {
                            origin: IssueOrigin::Sale(SaleCostOrigin::Invoice(line)),
                            ..
                        } => format!("Sale line {line}"),
                        Movement::Issue {
                            origin: IssueOrigin::Sale(SaleCostOrigin::Delivery(line)),
                            ..
                        } => format!("Delivery note line {line}"),
                        Movement::Issue {
                            origin: IssueOrigin::Sale(SaleCostOrigin::ReturnCounter(id)),
                            ..
                        } => format!("Customer return reversal movement {id}"),
                        Movement::Issue {
                            origin: IssueOrigin::SupplierReturn(line),
                            ..
                        } => {
                            format!("Supplier return movement {line}")
                        }
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
                if let Movement::Issue {
                    origin,
                    retain_cost,
                } = issue
                {
                    if let (IssueOrigin::Sale(origin), Some(reported_sale_costs)) =
                        (origin, reported_sale_costs.as_deref_mut())
                    {
                        reported_sale_costs
                            .insert(*origin, portions.iter().map(|layer| layer.value).sum());
                    }
                    if *retain_cost {
                        pool.issue_layers.insert(*origin, portions);
                    }
                }
            }
        }
    }
    Ok(Ok(pool))
}

fn ageing_breakdown(
    pool: Pool,
    as_of: NaiveDate,
    cutoffs: [u32; 3],
    valued: bool,
) -> StockAgeingBreakdown {
    let mut by_date = BTreeMap::<_, Layer>::new();
    if let Some(layers) = pool.fifo_layers {
        for (date, layer) in layers {
            let total = by_date.entry(date).or_default();
            total.quantity += layer.quantity;
            total.value += layer.value;
        }
    }
    let mut bands = [AgeingAmount {
        quantity: 0,
        fifo_value: valued.then_some(0),
    }; 4];
    let mut receipts = Vec::with_capacity(by_date.len());
    for (received_on, layer) in by_date {
        let age_days = (as_of - received_on).num_days();
        let band = cutoffs
            .iter()
            .position(|&limit| age_days <= i64::from(limit))
            .unwrap_or(3);
        bands[band].quantity += layer.quantity;
        if let Some(value) = &mut bands[band].fifo_value {
            *value += layer.value;
        }
        receipts.push(StockAgeingReceipt {
            received_on,
            age_days,
            amount: AgeingAmount {
                quantity: layer.quantity,
                fifo_value: valued.then_some(layer.value),
            },
        });
    }
    StockAgeingBreakdown {
        bands,
        receipts,
        fifo_value: valued.then_some(pool.available.value),
    }
}

fn journal_events(
    conn: &Connection,
    company: i64,
    end: NaiveDate,
    events: &mut HashMap<i64, Vec<Event>>,
) -> Result<()> {
    struct ItemMovement {
        date: NaiveDate,
        adjustment: i64,
        quantity: i128,
        issued: bool,
        receipt_cost: Option<i128>,
    }

    let mut stmt = conn.prepare(
        "SELECT j.id, j.date, e.item_id, e.quantity, e.amount, (
             SELECT MIN(a.adjustment_id) FROM stock_journal_allocations a WHERE a.entry_id = e.id
         )
         FROM stock_journals j JOIN stock_journal_entries e ON e.journal_id = j.id
         WHERE j.company_id = ?1 AND j.date <= ?2",
    )?;
    let mut rows = stmt.query(params![company, end.to_string()])?;
    let mut movements = HashMap::<(i64, i64), ItemMovement>::new();
    while let Some(row) = rows.next()? {
        let journal = row.get(0)?;
        let item = row.get(2)?;
        let quantity = row.get::<_, i64>(3)?;
        let adjustment = row.get::<_, i64>(5)?;
        let movement = match movements.entry((journal, item)) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(ItemMovement {
                date: parse_date(&row.get::<_, String>(1)?)?,
                adjustment,
                quantity: 0,
                issued: false,
                receipt_cost: Some(0),
            }),
        };
        movement.adjustment = movement.adjustment.min(adjustment);
        movement.quantity += i128::from(quantity);
        if quantity < 0 {
            movement.issued = true;
        } else {
            let cost = row.get::<_, Option<i64>>(4)?;
            movement.receipt_cost = movement
                .receipt_cost
                .zip(cost)
                .map(|(sum, cost)| sum + i128::from(cost));
        }
    }
    for ((journal_id, item), saved) in movements {
        let movement = match saved.quantity.cmp(&0) {
            std::cmp::Ordering::Equal => continue,
            std::cmp::Ordering::Less => Movement::AdjustmentOut,
            std::cmp::Ordering::Greater => Movement::JournalReceipt {
                journal_id,
                amount: if saved.issued {
                    None
                } else {
                    saved.receipt_cost
                },
            },
        };
        events.entry(item).or_default().push(Event {
            date: saved.date,
            source_id: saved.adjustment,
            quantity: saved.quantity.abs(),
            movement,
        });
    }
    Ok(())
}

fn events(conn: &Connection, company: i64, end: NaiveDate) -> Result<HashMap<i64, Vec<Event>>> {
    let mut events = HashMap::<i64, Vec<Event>>::new();
    let retained_costs = return_events(conn, company, end, &mut events)?;
    journal_events(conn, company, end, &mut events)?;
    {
        let mut stmt = conn.prepare(
            "SELECT a.id, a.item_id, a.date, a.quantity, (
                SELECT c.amount FROM stock_receipt_costs c WHERE c.adjustment_id = a.id ORDER BY c.id DESC LIMIT 1
             ), l.id, n.reversed_on, l.reversal_adjustment_id
             FROM stock_adjustments a
             LEFT JOIN stock_note_lines l ON l.adjustment_id = a.id
             LEFT JOIN stock_notes n ON n.id = l.note_id AND n.company_id = a.company_id
             WHERE a.company_id = ?1
             AND NOT EXISTS(SELECT 1 FROM stock_journal_allocations j WHERE j.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM invoice_reversal_stock r WHERE r.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_note_lines r WHERE r.reversal_adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_return_lines r WHERE r.adjustment_id = a.id OR r.reversal_adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM invoice_return_lines r WHERE r.adjustment_id = a.id OR r.reversal_adjustment_id = a.id)",
        )?;
        let mut rows = stmt.query([company])?;
        while let Some(row) = rows.next()? {
            let date = parse_date(&row.get::<_, String>(2)?)?;
            if date > end {
                continue;
            }
            let quantity = i128::from(row.get::<_, i64>(3)?);
            let delivery = row.get::<_, Option<i64>>(5)?;
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
            } else if let Some(line) = delivery {
                Movement::Issue {
                    origin: IssueOrigin::Sale(SaleCostOrigin::Delivery(line)),
                    retain_cost: reversal.is_some()
                        || retained_costs.contains(&SaleCostOrigin::Delivery(line)),
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
                    Movement::IssueReversal(IssueOrigin::Sale(SaleCostOrigin::Delivery(
                        delivery.ok_or_else(|| {
                            Error::Validation(
                                "Reversed delivery is missing its original movement".into(),
                            )
                        })?,
                    )))
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
            "SELECT l.id, l.item_id, i.kind, v.date, l.quantity, l.amount, rv.date,
                    COALESCE(l.billed_quantity, l.quantity)
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
                        origin: IssueOrigin::Sale(SaleCostOrigin::Invoice(line)),
                        retain_cost: reversal.is_some_and(|date| date <= end)
                            || retained_costs.contains(&SaleCostOrigin::Invoice(line)),
                    },
                    InvoiceKind::Purchase => {
                        Movement::Purchase(crate::invoice_stock_links::physical_net(
                            conn,
                            line,
                            row.get(7)?,
                            row.get(5)?,
                        )?)
                    }
                },
            });
            if let Some(date) = reversal.filter(|date| *date <= end) {
                item_events.push(Event {
                    date,
                    source_id: line,
                    quantity,
                    movement: match kind {
                        InvoiceKind::Sale => Movement::IssueReversal(IssueOrigin::Sale(
                            SaleCostOrigin::Invoice(line),
                        )),
                        InvoiceKind::Purchase => Movement::PurchaseReversal,
                    },
                });
            }
        }
    }
    Ok(events)
}

fn return_events(
    conn: &Connection,
    company: i64,
    end: NaiveDate,
    events: &mut HashMap<i64, Vec<Event>>,
) -> Result<HashSet<SaleCostOrigin>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.quantity, l.adjustment_id, l.reversal_adjustment_id, r.date, r.reversed_on,
                src.id, src.item_id, src.quantity, src.adjustment_id, n.kind, n.date, NULL, NULL, NULL
         FROM stock_return_lines l JOIN stock_returns r ON r.id = l.return_id
         JOIN stock_note_lines src ON src.id = l.note_line_id
         JOIN stock_notes n ON n.id = src.note_id AND n.company_id = r.company_id
         WHERE r.company_id = ?1
         UNION ALL
         SELECT r.id, r.quantity, r.adjustment_id, r.reversal_adjustment_id, r.date, r.reversed_on,
                COALESCE(r.delivery_line, r.line_id), r.item_id, COALESCE(r.delivery_quantity, r.invoice_quantity),
                COALESCE(r.delivery_adjustment_id, r.line_id), r.kind, COALESCE(r.delivery_date, r.invoice_date),
                r.line_id, r.invoice_quantity, r.delivery_quantity
         FROM invoice_stock_return_history r WHERE r.company_id = ?1",
    )?;
    let mut rows = stmt.query([company])?;
    let mut retained_costs = HashSet::new();
    while let Some(row) = rows.next()? {
        let date = parse_date(&row.get::<_, String>(4)?)?;
        if date > end {
            continue;
        }
        let reversal = row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        if reversal == Some(date) {
            continue;
        }
        let reversal = reversal.filter(|day| *day <= end);
        let kind = InvoiceKind::parse(&row.get::<_, String>(10)?)?;
        let id = row.get(0)?;
        let movement_id = row.get(2)?;
        let quantity = row.get::<_, i64>(1)?;
        let supplier_origin = IssueOrigin::SupplierReturn(movement_id);
        let movement = if kind == InvoiceKind::Sale {
            let physical_line = row.get(6)?;
            let invoice_line = row.get::<_, Option<i64>>(12)?;
            let delivery_quantity = row.get::<_, Option<i64>>(14)?;
            let origin = match (invoice_line, delivery_quantity) {
                (Some(line), None) => SaleCostOrigin::Invoice(line),
                _ => SaleCostOrigin::Delivery(physical_line),
            };
            let share = if let Some(line) = invoice_line {
                crate::invoice_returns::cost_share(
                    conn,
                    id,
                    line,
                    row.get(13)?,
                    quantity,
                    delivery_quantity,
                )?
            } else {
                crate::stock_returns::cost_share(conn, id, row.get(8)?, quantity)?
            };
            retained_costs.insert(origin);
            let source_date = parse_date(&row.get::<_, String>(11)?)?;
            Movement::SaleReturn {
                origin: IssueOrigin::Sale(origin),
                same_day_adjustment: (source_date == date).then_some(row.get(9)?),
                share,
            }
        } else {
            Movement::Issue {
                origin: supplier_origin,
                retain_cost: reversal.is_some(),
            }
        };
        let item_events = events.entry(row.get(7)?).or_default();
        item_events.push(Event {
            date,
            source_id: movement_id,
            quantity: i128::from(quantity),
            movement,
        });
        if let Some(date) = reversal {
            let counter_id = row.get(3)?;
            let movement = match kind {
                InvoiceKind::Sale if row.get::<_, Option<i64>>(12)?.is_some() => Movement::Issue {
                    origin: IssueOrigin::Sale(SaleCostOrigin::ReturnCounter(counter_id)),
                    retain_cost: false,
                },
                InvoiceKind::Sale => Movement::AdjustmentOut,
                InvoiceKind::Purchase => Movement::IssueReversal(supplier_origin),
            };
            item_events.push(Event {
                date,
                source_id: counter_id,
                quantity: i128::from(quantity),
                movement,
            });
        }
    }
    Ok(retained_costs)
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
             AND NOT EXISTS(SELECT 1 FROM stock_journal_allocations j WHERE j.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM invoice_reversal_stock r WHERE r.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_note_lines r WHERE r.reversal_adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_return_lines r WHERE r.adjustment_id = a.id OR r.reversal_adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM invoice_return_lines r WHERE r.adjustment_id = a.id OR r.reversal_adjustment_id = a.id)
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
             AND NOT EXISTS(SELECT 1 FROM stock_journal_allocations j WHERE j.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM invoice_reversal_stock r WHERE r.adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_note_lines r WHERE r.reversal_adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM stock_return_lines r WHERE r.adjustment_id = a.id OR r.reversal_adjustment_id = a.id)
             AND NOT EXISTS(SELECT 1 FROM invoice_return_lines r WHERE r.adjustment_id = a.id OR r.reversal_adjustment_id = a.id)",
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
                Calculation::Valuation(method),
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

    /// Age the remaining company-wide FIFO layers by their latest receipt dates.
    pub fn stock_ageing(
        &self,
        company: i64,
        as_of: &str,
        cutoffs: [u32; 3],
    ) -> Result<StockAgeingReport> {
        let date = parse_date(as_of)?;
        if !(cutoffs[0] < cutoffs[1] && cutoffs[1] < cutoffs[2]) {
            return Err(Error::Validation(
                "Enter three increasing age cutoffs in days".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut movements = events(&tx, company, date)?;
        let balances = self.stock_balances(company, Some(as_of))?;
        let mut rows = Vec::new();
        for balance in balances {
            let mut item_events = movements.remove(&balance.item.id).unwrap_or_default();
            let (pool, issue, valued) = match calculate(
                &mut item_events,
                Calculation::Valuation(ActualCostMethod::Fifo),
                None,
            )? {
                Ok(pool) => (Some(pool), None, true),
                Err(
                    cost_issue @ (ActualCostIssue::MissingReceiptCost { .. }
                    | ActualCostIssue::MissingJournalCost { .. }),
                ) => match calculate(&mut item_events, Calculation::AgeingQuantities, None)? {
                    Ok(pool) => (Some(pool), Some(cost_issue), false),
                    Err(quantity_issue) => (None, Some(quantity_issue), false),
                },
                Err(issue) => (None, Some(issue), false),
            };
            let breakdown = if let Some(pool) = pool {
                if pool.available.quantity != balance.quantity {
                    return Err(Error::Validation(format!(
                        "Stock movements do not reconcile for {}",
                        balance.item.name,
                    )));
                }
                Some(ageing_breakdown(pool, date, cutoffs, valued))
            } else {
                None
            };
            if balance.quantity != 0 || issue.is_some() {
                rows.push(StockAgeingRow {
                    item: balance.item,
                    quantity: balance.quantity,
                    breakdown,
                    issue,
                });
            }
        }
        tx.commit()?;
        Ok(StockAgeingReport {
            date,
            cutoffs,
            rows,
        })
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
                Calculation::Valuation(method),
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
            let cost = if sale.awaiting_stock {
                None
            } else if let Some(id) = sale.counter_issue {
                costs.get(&SaleCostOrigin::ReturnCounter(id)).copied()
            } else {
                sale.cost_sources
                    .iter()
                    .try_fold(0_i128, |total, source| {
                        costs
                            .get(&source.origin)
                            .map(|&cost| total + source.portion(cost))
                    })
                    .map(|cost| sale.signed_cost(cost))
            };
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
                issue = Some(if sale.awaiting_stock {
                    ActualCostIssue::PendingStockIssue
                } else {
                    issues.get(&sale.item_id).cloned().ok_or_else(|| {
                        Error::Validation(format!(
                            "Sale line {} has no matching cost event",
                            sale.line_id
                        ))
                    })?
                });
                report.unavailable_lines += 1;
            }
            report.rows.push(ActualSalesCostRow { sale, cost, issue });
        }
        tx.commit()?;
        Ok(report)
    }
}
