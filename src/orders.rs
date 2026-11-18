//! Sales and purchase orders fulfilled by linked invoices.
use crate::inventory::{
    check_quantity_precision, insert_invoice_on_connection, ledger_category, line_amount,
    InvoiceDraft, InvoiceKind, StockItem,
};
use crate::{parse_date, required, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, types::Type, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{HashMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE orders(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            kind TEXT NOT NULL CHECK(kind IN ('Sale', 'Purchase')),
            party_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            date TEXT NOT NULL,
            expected_on TEXT,
            notes TEXT NOT NULL,
            closed_at TEXT,
            close_reason TEXT,
            CHECK((closed_at IS NULL) = (close_reason IS NULL)),
            UNIQUE(company_id, kind, party_ledger_id, reference_key)
        );
        CREATE INDEX orders_company_date ON orders(company_id, date, id);
        CREATE TABLE order_lines(
            id INTEGER PRIMARY KEY,
            order_id INTEGER NOT NULL REFERENCES orders(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            unit_price INTEGER NOT NULL CHECK(unit_price >= 0),
            UNIQUE(order_id, item_id)
        );
        CREATE TABLE invoice_order_links(
            invoice_id INTEGER PRIMARY KEY REFERENCES invoices(id),
            order_id INTEGER NOT NULL REFERENCES orders(id)
        );
        CREATE INDEX invoice_order_links_order ON invoice_order_links(order_id);
        CREATE VIEW order_line_progress AS
            SELECT ol.id, ol.order_id, ol.item_id, ol.quantity, ol.unit_price, COALESCE((
                SELECT SUM(il.quantity)
                FROM invoice_order_links link JOIN invoice_lines il ON il.invoice_id = link.invoice_id
                WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
            ), 0) AS fulfilled
            FROM order_lines ol;",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct OrderLine {
    pub item_id: i64,
    pub quantity: i64,
    pub unit_price: i64,
}

#[derive(Debug, Clone)]
pub struct OrderDraft {
    pub kind: InvoiceKind,
    pub party_ledger_id: i64,
    pub reference: String,
    pub date: String,
    pub expected_on: Option<String>,
    pub notes: String,
    pub lines: Vec<OrderLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Open,
    Partial,
    Fulfilled,
    Closed,
}
impl OrderStatus {
    pub const ALL: [Self; 4] = [Self::Open, Self::Partial, Self::Fulfilled, Self::Closed];

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Partial => "Partially fulfilled",
            Self::Fulfilled => "Fulfilled",
            Self::Closed => "Closed",
        }
    }

    pub fn has_pending(self) -> bool {
        matches!(self, Self::Open | Self::Partial)
    }
}

#[derive(Debug, Clone)]
pub struct OrderSummary {
    pub id: i64,
    pub kind: InvoiceKind,
    pub party_ledger_id: i64,
    pub party: String,
    pub reference: String,
    pub date: String,
    pub expected_on: Option<String>,
    pub notes: String,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
    pub status: OrderStatus,
}

#[derive(Debug, Clone)]
pub struct OrderLineProgress {
    pub line: OrderLine,
    pub item: String,
    pub unit: String,
    pub fulfilled: i64,
}

#[derive(Debug, Clone)]
pub struct OrderAvailability {
    pub item: StockItem,
    pub on_hand: i128,
    pub open_sales: i128,
    pub open_purchases: i128,
}

impl OrderAvailability {
    pub fn after_sales(&self) -> i128 {
        self.on_hand - self.open_sales
    }

    pub fn projected(&self) -> i128 {
        self.after_sales() + self.open_purchases
    }
}

#[derive(Debug, Clone)]
pub struct PendingOrderLine {
    pub order_id: i64,
    pub kind: InvoiceKind,
    pub reference: String,
    pub party: String,
    pub expected_on: Option<NaiveDate>,
    pub item_id: i64,
    pub item: String,
    pub unit: String,
    pub remaining: i64,
}
impl OrderLineProgress {
    pub fn unfulfilled(&self) -> i64 {
        self.line.quantity - self.fulfilled
    }
}

#[derive(Debug, Clone)]
pub struct OrderInvoice {
    pub id: i64,
    pub date: String,
    pub reference: String,
    pub reversal_date: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrderDetail {
    pub summary: OrderSummary,
    pub lines: Vec<OrderLineProgress>,
    pub invoices: Vec<OrderInvoice>,
    pub total: i128,
}

const ORDER_SUMMARIES: &str =
    "SELECT o.id, o.kind, o.party_ledger_id, l.name, o.reference, o.date,
            o.expected_on, o.notes, o.closed_at, o.close_reason,
            EXISTS(SELECT 1 FROM order_line_progress p WHERE p.order_id = o.id AND p.fulfilled < p.quantity),
            EXISTS(SELECT 1 FROM invoice_order_links link WHERE link.order_id = o.id
                AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id))
     FROM orders o JOIN ledgers l ON l.id = o.party_ledger_id AND l.company_id = o.company_id
     WHERE o.company_id = ?1";

fn read_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<OrderSummary> {
    let raw_kind: String = row.get(1)?;
    let kind = InvoiceKind::parse(&raw_kind)
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(err)))?;
    let closed_at: Option<String> = row.get(8)?;
    let pending: bool = row.get(10)?;
    let has_invoices: bool = row.get(11)?;
    let status = if closed_at.is_some() {
        OrderStatus::Closed
    } else if !pending {
        OrderStatus::Fulfilled
    } else if has_invoices {
        OrderStatus::Partial
    } else {
        OrderStatus::Open
    };
    Ok(OrderSummary {
        id: row.get(0)?,
        kind,
        party_ledger_id: row.get(2)?,
        party: row.get(3)?,
        reference: row.get(4)?,
        date: row.get(5)?,
        expected_on: row.get(6)?,
        notes: row.get(7)?,
        closed_at,
        close_reason: row.get(9)?,
        status,
    })
}

fn order_summary(conn: &Connection, company: i64, order: i64) -> Result<OrderSummary> {
    conn.query_row(
        &format!("{ORDER_SUMMARIES} AND o.id = ?2"),
        params![company, order],
        read_summary,
    )
    .optional()?
    .ok_or_else(|| Error::Validation("Order not found in this company".into()))
}

fn order_lines(conn: &Connection, order: i64) -> Result<Vec<OrderLineProgress>> {
    let mut stmt = conn.prepare(
        "SELECT p.item_id, p.quantity, p.unit_price, s.name, s.unit, p.fulfilled
         FROM order_line_progress p JOIN stock_items s ON s.id = p.item_id
         WHERE p.order_id = ?1 ORDER BY p.id",
    )?;
    let rows = stmt.query_map([order], |row| {
        Ok(OrderLineProgress {
            line: OrderLine {
                item_id: row.get(0)?,
                quantity: row.get(1)?,
                unit_price: row.get(2)?,
            },
            item: row.get(3)?,
            unit: row.get(4)?,
            fulfilled: row.get(5)?,
        })
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty()
        || rows
            .iter()
            .any(|row| !(0..=row.line.quantity).contains(&row.fulfilled))
    {
        return Err(Error::Validation(
            "Order quantities are inconsistent".into(),
        ));
    }
    Ok(rows)
}

pub(crate) fn linked_order(conn: &Connection, invoice: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT order_id FROM invoice_order_links WHERE invoice_id = ?1",
            [invoice],
            |row| row.get(0),
        )
        .optional()?)
}

impl Store {
    /// Stock quantities and open order remainders across all saved dates.
    pub fn order_availability(&self, company: i64) -> Result<Vec<OrderAvailability>> {
        let snapshot = self.conn.unchecked_transaction()?;
        let balances = self.stock_balances(company, None)?;
        let mut commitments = HashMap::<i64, (i128, i128)>::new();
        {
            let mut stmt = snapshot.prepare(
                "SELECT p.item_id, o.kind, p.quantity, p.fulfilled
                 FROM orders o JOIN order_line_progress p ON p.order_id = o.id
                 WHERE o.company_id = ?1 AND o.closed_at IS NULL",
            )?;
            let rows = stmt.query_map([company], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (item, kind, ordered, fulfilled) = row?;
                if !(0..=ordered).contains(&fulfilled) {
                    return Err(Error::Validation(
                        "Order quantities are inconsistent".into(),
                    ));
                }
                let (sales, purchases) = commitments.entry(item).or_default();
                let remaining = i128::from(ordered - fulfilled);
                match InvoiceKind::parse(&kind)? {
                    InvoiceKind::Sale => *sales += remaining,
                    InvoiceKind::Purchase => *purchases += remaining,
                }
            }
        }
        snapshot.commit()?;
        Ok(balances
            .into_iter()
            .map(|balance| {
                let (open_sales, open_purchases) = commitments
                    .get(&balance.item.id)
                    .copied()
                    .unwrap_or_default();
                OrderAvailability {
                    item: balance.item,
                    on_hand: balance.quantity,
                    open_sales,
                    open_purchases,
                }
            })
            .collect())
    }

    /// Current unfulfilled order lines, optionally limited by expected date.
    pub fn pending_order_schedule(
        &self,
        company: i64,
        kind: Option<InvoiceKind>,
        expected_through: Option<&str>,
    ) -> Result<Vec<PendingOrderLine>> {
        let through = expected_through.map(parse_date).transpose()?;
        let mut stmt = self.conn.prepare(
            "SELECT o.id, o.kind, o.reference, l.name, o.expected_on,
                    p.item_id, s.name, s.unit, p.quantity, p.fulfilled
             FROM orders o
             JOIN order_line_progress p ON p.order_id = o.id
             JOIN ledgers l ON l.id = o.party_ledger_id AND l.company_id = o.company_id
             JOIN stock_items s ON s.id = p.item_id AND s.company_id = o.company_id
             WHERE o.company_id = ?1 AND o.closed_at IS NULL AND p.fulfilled != p.quantity
               AND (?2 IS NULL OR o.kind = ?2)
             ORDER BY o.id, p.id",
        )?;
        let rows = stmt.query_map(params![company, kind.map(InvoiceKind::as_str)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })?;
        let mut schedule = Vec::new();
        for row in rows {
            let (order_id, kind, reference, party, date, item_id, item, unit, ordered, fulfilled) =
                row?;
            if !(0..=ordered).contains(&fulfilled) {
                return Err(Error::Validation(
                    "Order quantities are inconsistent".into(),
                ));
            }
            let expected_on = date.as_deref().map(parse_date).transpose()?;
            if through.is_some_and(|limit| expected_on.is_none_or(|date| date > limit)) {
                continue;
            }
            schedule.push(PendingOrderLine {
                order_id,
                kind: InvoiceKind::parse(&kind)?,
                reference,
                party,
                expected_on,
                item_id,
                item,
                unit,
                remaining: ordered - fulfilled,
            });
        }
        schedule.sort_by_key(|line| (line.expected_on.is_none(), line.expected_on));
        Ok(schedule)
    }

    pub fn create_order(&mut self, company: i64, draft: &OrderDraft) -> Result<i64> {
        let reference = required(&draft.reference, "Order reference")?;
        let reference_key = reference.to_lowercase();
        let date = parse_date(&draft.date)?;
        let expected = draft.expected_on.as_deref().map(parse_date).transpose()?;
        if expected.is_some_and(|expected| expected < date) {
            return Err(Error::Validation(
                "Expected date cannot precede the order date".into(),
            ));
        }
        if draft.lines.is_empty() {
            return Err(Error::Validation("Enter at least one stock item".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !matches!(
            ledger_category(&tx, company, draft.party_ledger_id)?,
            Category::Asset | Category::Liability
        ) {
            return Err(Error::Validation(
                "The party ledger must be an asset or liability".into(),
            ));
        }
        let mut items = HashSet::new();
        for line in &draft.lines {
            if !items.insert(line.item_id) {
                return Err(Error::Validation("Enter each item once per order".into()));
            }
            line_amount(line.quantity, line.unit_price)?;
            check_quantity_precision(&tx, company, line.item_id, line.quantity)?;
        }
        let inserted = tx.execute(
            "INSERT INTO orders(company_id, kind, party_ledger_id, reference, reference_key, date, expected_on, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(company_id, kind, party_ledger_id, reference_key) DO NOTHING",
            params![company, draft.kind.as_str(), draft.party_ledger_id, reference, reference_key,
                date.format("%Y-%m-%d").to_string(), expected.map(|d| d.format("%Y-%m-%d").to_string()), draft.notes.trim()],
        )?;
        if inserted == 0 {
            return Err(Error::Validation(
                "An order with this type, party and reference already exists".into(),
            ));
        }
        let order = tx.last_insert_rowid();
        for line in &draft.lines {
            tx.execute(
                "INSERT INTO order_lines(order_id, item_id, quantity, unit_price) VALUES (?1, ?2, ?3, ?4)",
                params![order, line.item_id, line.quantity, line.unit_price],
            )?;
        }
        tx.commit()?;
        Ok(order)
    }

    pub fn orders(&self, company: i64) -> Result<Vec<OrderSummary>> {
        let mut stmt = self.conn.prepare(&format!(
            "{ORDER_SUMMARIES} ORDER BY o.date DESC, o.id DESC"
        ))?;
        let rows = stmt.query_map([company], read_summary)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn order_detail(&self, company: i64, id: i64) -> Result<OrderDetail> {
        let tx = self.conn.unchecked_transaction()?;
        let summary = order_summary(&tx, company, id)?;
        let lines = order_lines(&tx, id)?;
        let invoices = {
            let mut stmt = tx.prepare(
                "SELECT i.id, v.date, v.reference, rv.date FROM invoice_order_links link
                 JOIN invoices i ON i.id = link.invoice_id AND i.company_id = ?1
                 JOIN vouchers v ON v.id = i.voucher_id
                 LEFT JOIN invoice_reversals r ON r.invoice_id = i.id
                 LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
                 WHERE link.order_id = ?2 ORDER BY v.date, i.id",
            )?;
            let rows = stmt.query_map(params![company, id], |row| {
                Ok(OrderInvoice {
                    id: row.get(0)?,
                    date: row.get(1)?,
                    reference: row.get(2)?,
                    reversal_date: row.get(3)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut total = 0_i128;
        for row in &lines {
            total += i128::from(line_amount(row.line.quantity, row.line.unit_price)?);
        }
        tx.commit()?;
        Ok(OrderDetail {
            summary,
            lines,
            invoices,
            total,
        })
    }

    pub fn close_order(&mut self, company: i64, id: i64, reason: &str) -> Result<()> {
        let reason = required(reason, "Reason for closing the order")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !order_summary(&tx, company, id)?.status.has_pending() {
            return Err(Error::Validation(
                "Only an order with pending quantities can be closed".into(),
            ));
        }
        tx.execute(
            "UPDATE orders SET closed_at = CURRENT_TIMESTAMP, close_reason = ?1 WHERE id = ?2 AND company_id = ?3",
            params![reason, id, company],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Post an invoice and count its stock quantities against one order atomically.
    pub fn invoice_order(&mut self, company: i64, id: i64, draft: &InvoiceDraft) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let order = order_summary(&tx, company, id)?;
        if !order.status.has_pending() {
            return Err(Error::Validation(
                "This order has no pending quantities to invoice".into(),
            ));
        }
        if draft.kind != order.kind || draft.party_ledger_id != order.party_ledger_id {
            return Err(Error::Validation(
                "Invoice type and party must match the order".into(),
            ));
        }
        let invoice_date = parse_date(&draft.date)?;
        if invoice_date < parse_date(&order.date)? {
            return Err(Error::Validation(
                "Invoice date cannot precede the order date".into(),
            ));
        }
        if draft.lines.is_empty() {
            return Err(Error::Validation(
                "Add an ordered item to fulfil this order".into(),
            ));
        }
        {
            let mut stmt = tx.prepare(
                "SELECT v.date FROM invoice_order_links link
                 JOIN invoice_reversals r ON r.invoice_id = link.invoice_id
                 JOIN vouchers v ON v.id = r.reversal_voucher_id
                 WHERE link.order_id = ?1",
            )?;
            for row in stmt.query_map([id], |row| row.get::<_, String>(0))? {
                if invoice_date < parse_date(&row?)? {
                    return Err(Error::Validation(
                        "Invoice date cannot precede an existing invoice reversal on the order"
                            .into(),
                    ));
                }
            }
        }
        let progress = order_lines(&tx, id)?;
        let mut remaining: HashMap<_, _> = progress
            .iter()
            .map(|row| (row.line.item_id, (row.unfulfilled(), row.item.as_str())))
            .collect();
        for line in &draft.lines {
            let (quantity, name) = remaining.get_mut(&line.item_id).ok_or_else(|| {
                Error::Validation(
                    "Every stock item on this invoice must appear on the order".into(),
                )
            })?;
            if line.quantity <= 0 || line.quantity > *quantity {
                return Err(Error::Validation(format!(
                    "Quantity for {name} exceeds the pending amount or is not positive"
                )));
            }
            *quantity -= line.quantity;
        }
        let (invoice, _) = insert_invoice_on_connection(&tx, company, draft)?;
        tx.execute(
            "INSERT INTO invoice_order_links(invoice_id, order_id) VALUES (?1, ?2)",
            params![invoice, id],
        )?;
        tx.commit()?;
        Ok(invoice)
    }
}
