//! Sales and purchase orders fulfilled by linked invoices.
use crate::inventory::{
    check_quantity_precision, insert_direct_invoice, ledger_category, line_amount,
    InvoiceAllocation, InvoiceDraft, InvoiceKind, StockInvoicePosting, StockItem,
};
use crate::{parse_date, required, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, types::Type, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashMap, HashSet};

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

pub(crate) fn migrate_line_closures(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE order_line_closures(
            id INTEGER PRIMARY KEY,
            order_line_id INTEGER NOT NULL REFERENCES order_lines(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            closed_on TEXT NOT NULL,
            reason TEXT NOT NULL,
            reopened_on TEXT,
            reopen_reason TEXT,
            CHECK((reopened_on IS NULL) = (reopen_reason IS NULL))
        );
        CREATE INDEX order_line_closures_line ON order_line_closures(order_line_id);
        CREATE VIEW order_line_availability AS
            SELECT p.*, COALESCE((SELECT SUM(c.quantity) FROM order_line_closures c
                WHERE c.order_line_id = p.id AND c.reopened_on IS NULL), 0) AS preclosed
            FROM order_line_progress p;",
    )?;
    Ok(())
}

pub(crate) fn migrate_amendments(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE order_revisions(
            id INTEGER PRIMARY KEY,
            order_id INTEGER NOT NULL REFERENCES orders(id),
            reference TEXT NOT NULL,
            expected_on TEXT,
            notes TEXT NOT NULL,
            amended_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            reason TEXT NOT NULL
        );
        CREATE INDEX order_revisions_order ON order_revisions(order_id, id);
        CREATE TABLE order_revision_lines(
            revision_id INTEGER NOT NULL REFERENCES order_revisions(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            item TEXT NOT NULL,
            unit TEXT NOT NULL,
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            unit_price INTEGER NOT NULL CHECK(unit_price >= 0),
            PRIMARY KEY(revision_id, item_id)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone)]
pub struct OrderAmendmentDraft {
    pub base_revision: Option<i64>,
    pub reference: String,
    pub expected_on: Option<String>,
    pub notes: String,
    pub lines: Vec<OrderLine>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct OrderRevision {
    pub id: i64,
    pub reference: String,
    pub expected_on: Option<String>,
    pub notes: String,
    pub amended_at: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct OrderRevisionLine {
    pub values: OrderLine,
    pub item: String,
    pub unit: String,
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
    pub revision: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct OrderLineProgress {
    pub line: OrderLine,
    pub item: String,
    pub unit: String,
    pub fulfilled: i64,
    pub billed: i64,
    pub preclosed: i64,
}

#[derive(Debug, Clone)]
pub struct OrderLineClosure {
    pub id: i64,
    pub item: String,
    pub unit: String,
    pub quantity: i64,
    pub closed_on: String,
    pub reason: String,
    pub reopened_on: Option<String>,
    pub reopen_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrderAvailability {
    pub item: StockItem,
    pub on_hand: i128,
    pub open_sales: i128,
    pub open_purchases: i128,
    pub reserved: i128,
    pub in_transit: i128,
}

impl OrderAvailability {
    pub fn unreserved(&self) -> i128 {
        self.on_hand - self.reserved - self.in_transit
    }

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
        self.line.quantity - self.fulfilled - self.preclosed
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
    pub line_closures: Vec<OrderLineClosure>,
    pub reservations: Vec<crate::stock_reservations::StockReservation>,
    pub total: i128,
}

impl OrderDetail {
    pub fn reserved_allocations(&self, item: i64) -> Vec<InvoiceAllocation> {
        let mut quantities = BTreeMap::<_, i64>::new();
        for hold in self
            .reservations
            .iter()
            .filter(|hold| hold.item_id == item && hold.remaining > 0)
        {
            *quantities
                .entry((hold.location_id, hold.batch_id))
                .or_default() += hold.remaining;
        }
        quantities
            .into_iter()
            .map(|((location_id, batch_id), quantity)| InvoiceAllocation {
                location_id,
                batch_id,
                quantity,
            })
            .collect()
    }
}

pub(crate) fn remaining_quantity(ordered: i64, fulfilled: i64, preclosed: i64) -> Result<i64> {
    if !(0..=ordered).contains(&fulfilled) || !(0..=ordered - fulfilled).contains(&preclosed) {
        return Err(Error::Validation(
            "Order quantities are inconsistent".into(),
        ));
    }
    Ok(ordered - fulfilled - preclosed)
}

const ORDER_SUMMARIES: &str =
    "SELECT o.id, o.kind, o.party_ledger_id, l.name, o.reference, o.date,
            o.expected_on, o.notes, o.closed_at, o.close_reason,
            EXISTS(SELECT 1 FROM order_line_availability p WHERE p.order_id = o.id AND p.quantity - p.fulfilled > p.preclosed),
            EXISTS(SELECT 1 FROM order_line_progress p WHERE p.order_id = o.id AND p.fulfilled > 0),
            EXISTS(SELECT 1 FROM order_lines ol JOIN order_line_closures c ON c.order_line_id = ol.id
                   WHERE ol.order_id = o.id AND c.reopened_on IS NULL),
            (SELECT MAX(r.id) FROM order_revisions r WHERE r.order_id = o.id)
     FROM orders o JOIN ledgers l ON l.id = o.party_ledger_id AND l.company_id = o.company_id
     WHERE o.company_id = ?1";

fn read_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<OrderSummary> {
    let raw_kind: String = row.get(1)?;
    let kind = InvoiceKind::parse(&raw_kind)
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(err)))?;
    let closed_at: Option<String> = row.get(8)?;
    let pending: bool = row.get(10)?;
    let has_fulfilment: bool = row.get(11)?;
    let has_preclosure: bool = row.get(12)?;
    let status = if closed_at.is_some() || (!pending && has_preclosure) {
        OrderStatus::Closed
    } else if !pending {
        OrderStatus::Fulfilled
    } else if has_fulfilment {
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
        revision: row.get(13)?,
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
        "SELECT p.item_id, p.quantity, p.unit_price, s.name, s.unit, p.fulfilled, p.billed, p.preclosed
         FROM order_line_availability p JOIN stock_items s ON s.id = p.item_id
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
            billed: row.get(6)?,
            preclosed: row.get(7)?,
        })
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty()
        || rows.iter().any(|row| {
            !(0..=row.fulfilled).contains(&row.billed)
                || remaining_quantity(row.line.quantity, row.fulfilled, row.preclosed).is_err()
        })
    {
        return Err(Error::Validation(
            "Order quantities are inconsistent".into(),
        ));
    }
    Ok(rows)
}

pub(crate) fn linked_order(conn: &Connection, invoice: i64) -> Result<Option<i64>> {
    Ok(linked_orders(conn, invoice)?.first().copied())
}

pub(crate) fn linked_orders(conn: &Connection, invoice: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT order_id FROM invoice_orders WHERE invoice_id = ?1 ORDER BY order_id",
    )?;
    let rows = stmt
        .query_map([invoice], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn expected_order_date(date: NaiveDate, expected: Option<&str>) -> Result<Option<NaiveDate>> {
    let expected = expected.map(parse_date).transpose()?;
    if expected.is_some_and(|expected| expected < date) {
        return Err(Error::Validation(
            "Expected date cannot precede the order date".into(),
        ));
    }
    Ok(expected)
}

fn validate_order_lines(conn: &Connection, company: i64, lines: &[OrderLine]) -> Result<()> {
    if lines.is_empty() {
        return Err(Error::Validation("Enter at least one stock item".into()));
    }
    let mut items = HashSet::new();
    for line in lines {
        if !items.insert(line.item_id) {
            return Err(Error::Validation("Enter each item once per order".into()));
        }
        line_amount(line.quantity, line.unit_price)?;
        check_quantity_precision(conn, company, line.item_id, line.quantity)?;
    }
    Ok(())
}

pub(crate) fn validate_order_draft(
    conn: &Connection,
    company: i64,
    draft: &OrderDraft,
) -> Result<(NaiveDate, Option<NaiveDate>)> {
    let date = parse_date(&draft.date)?;
    let expected = expected_order_date(date, draft.expected_on.as_deref())?;
    if !matches!(
        ledger_category(conn, company, draft.party_ledger_id)?,
        Category::Asset | Category::Liability
    ) {
        return Err(Error::Validation(
            "The party ledger must be an asset or liability".into(),
        ));
    }
    validate_order_lines(conn, company, &draft.lines)?;
    Ok((date, expected))
}

pub(crate) fn insert_order(conn: &Connection, company: i64, draft: &OrderDraft) -> Result<i64> {
    let reference = required(&draft.reference, "Order reference")?;
    let reference_key = reference.to_lowercase();
    let (date, expected) = validate_order_draft(conn, company, draft)?;
    let inserted = conn.execute(
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
    let order = conn.last_insert_rowid();
    for line in &draft.lines {
        conn.execute(
                "INSERT INTO order_lines(order_id, item_id, quantity, unit_price) VALUES (?1, ?2, ?3, ?4)",
                params![order, line.item_id, line.quantity, line.unit_price],
            )?;
    }
    Ok(order)
}

fn remove_unused_line(conn: &Connection, order: i64, item: i64) -> Result<bool> {
    let removed = conn.execute(
        "DELETE FROM order_lines AS ol WHERE ol.order_id = ?1 AND ol.item_id = ?2
           AND NOT EXISTS(SELECT 1 FROM order_line_closures c WHERE c.order_line_id = ol.id)
           AND NOT EXISTS(SELECT 1 FROM stock_reservations r WHERE r.order_line_id = ol.id)
           AND NOT EXISTS(
               SELECT 1 FROM stock_notes n JOIN stock_note_lines nl ON nl.note_id = n.id
               WHERE n.order_id = ol.order_id AND nl.item_id = ol.item_id
           )
           AND NOT EXISTS(
               SELECT 1 FROM invoice_order_links i JOIN invoice_lines il ON il.invoice_id = i.invoice_id
               WHERE i.order_id = ol.order_id AND il.item_id = ol.item_id
           )",
        params![order, item],
    )?;
    Ok(removed == 1)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FulfilmentChange {
    NewDocument,
    ReverseReturn,
}

fn check_order_stock_release_date(conn: &Connection, order: i64, date: NaiveDate) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT v.date FROM invoice_order_links link
         JOIN invoice_reversals r ON r.invoice_id = link.invoice_id
         JOIN vouchers v ON v.id = r.reversal_voucher_id
         WHERE link.order_id = ?1
           AND NOT EXISTS(SELECT 1 FROM stock_note_invoices n WHERE n.invoice_id = link.invoice_id)
         UNION ALL
         SELECT reversed_on FROM stock_notes WHERE order_id = ?1 AND reversed_on IS NOT NULL
         UNION ALL
         SELECT r.date FROM stock_returns r JOIN stock_notes n ON n.id = r.note_id WHERE n.order_id = ?1
         UNION ALL
         SELECT DISTINCT r.date FROM invoice_stock_return_history r
         LEFT JOIN stock_note_lines nl ON nl.id = r.delivery_line
         LEFT JOIN stock_notes n ON n.id = nl.note_id
         LEFT JOIN invoice_order_links i ON i.invoice_id = r.invoice_id AND r.delivery_line IS NULL
         WHERE COALESCE(n.order_id, i.order_id) = ?1",
    )?;
    for row in stmt.query_map([order], |row| row.get::<_, String>(0))? {
        if date < parse_date(&row?)? {
            return Err(Error::Validation(
                "The date cannot precede an existing stock return or reversal on the order".into(),
            ));
        }
    }
    Ok(())
}

fn check_line_reopen_date(conn: &Connection, order: i64, item: i64, date: NaiveDate) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT c.reopened_on FROM order_line_closures c
         JOIN order_lines ol ON ol.id = c.order_line_id
         WHERE ol.order_id = ?1 AND ol.item_id = ?2 AND c.reopened_on IS NOT NULL",
    )?;
    for row in stmt.query_map(params![order, item], |row| row.get::<_, String>(0))? {
        if date < parse_date(&row?)? {
            return Err(Error::Validation(
                "The date cannot precede the latest reopening of this order line".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn check_fulfilment(
    conn: &Connection,
    company: i64,
    id: i64,
    kind: InvoiceKind,
    party: i64,
    date: NaiveDate,
    change: FulfilmentChange,
    items: impl IntoIterator<Item = (i64, i64)>,
) -> Result<()> {
    let order = order_summary(conn, company, id)?;
    if change == FulfilmentChange::NewDocument && !order.status.has_pending() {
        return Err(Error::Validation(
            "This order has no unfulfilled quantities".into(),
        ));
    }
    if kind != order.kind || party != order.party_ledger_id {
        return Err(Error::Validation(
            "Document type and party must match the order".into(),
        ));
    }
    check_order_quantities(conn, &order, date, items)
}

fn check_order_quantities(
    conn: &Connection,
    order: &OrderSummary,
    date: NaiveDate,
    items: impl IntoIterator<Item = (i64, i64)>,
) -> Result<()> {
    if date < parse_date(&order.date)? {
        return Err(Error::Validation(
            "The date cannot precede the order date".into(),
        ));
    }
    check_order_stock_release_date(conn, order.id, date)?;
    let progress = order_lines(conn, order.id)?;
    let mut remaining: HashMap<_, _> = progress
        .iter()
        .map(|row| (row.line.item_id, (row.unfulfilled(), row.item.as_str())))
        .collect();
    let mut items = items.into_iter().peekable();
    if items.peek().is_none() {
        return Err(Error::Validation("Add an item to fulfil this order".into()));
    }
    for (item, quantity) in items {
        let (pending, name) = remaining
            .get_mut(&item)
            .ok_or_else(|| Error::Validation("Every stock item must appear on the order".into()))?;
        if quantity <= 0 || quantity > *pending {
            return Err(Error::Validation(format!(
                "Quantity for {name} exceeds the pending amount or is not positive"
            )));
        }
        check_line_reopen_date(conn, order.id, item, date)?;
        *pending -= quantity;
    }
    Ok(())
}

impl Store {
    /// Stock quantities and open order remainders across all saved dates.
    pub fn order_availability(&self, company: i64) -> Result<Vec<OrderAvailability>> {
        let snapshot = self.conn.unchecked_transaction()?;
        let rows = self.order_availability_in_snapshot(company)?;
        snapshot.commit()?;
        Ok(rows)
    }

    pub(crate) fn order_availability_in_snapshot(
        &self,
        company: i64,
    ) -> Result<Vec<OrderAvailability>> {
        let balances = self.stock_balances(company, None)?;
        let reserved = crate::stock_reservations::reserved_by_item(&self.conn, company)?;
        let in_transit = crate::stock_transit::in_transit_by_item(&self.conn, company)?;
        let mut commitments = HashMap::<i64, (i128, i128)>::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT p.item_id, o.kind, p.quantity, p.fulfilled, p.preclosed
                 FROM orders o JOIN order_line_availability p ON p.order_id = o.id
                 WHERE o.company_id = ?1 AND o.closed_at IS NULL",
            )?;
            let rows = stmt.query_map([company], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?;
            for row in rows {
                let (item, kind, ordered, fulfilled, preclosed) = row?;
                let (sales, purchases) = commitments.entry(item).or_default();
                let remaining = i128::from(remaining_quantity(ordered, fulfilled, preclosed)?);
                match InvoiceKind::parse(&kind)? {
                    InvoiceKind::Sale => *sales += remaining,
                    InvoiceKind::Purchase => *purchases += remaining,
                }
            }
        }
        Ok(balances
            .into_iter()
            .map(|balance| {
                let (open_sales, open_purchases) = commitments
                    .get(&balance.item.id)
                    .copied()
                    .unwrap_or_default();
                OrderAvailability {
                    reserved: reserved.get(&balance.item.id).copied().unwrap_or_default(),
                    in_transit: in_transit
                        .get(&balance.item.id)
                        .copied()
                        .unwrap_or_default(),
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
                    p.item_id, s.name, s.unit, p.quantity, p.fulfilled, p.preclosed
             FROM orders o
             JOIN order_line_availability p ON p.order_id = o.id
             JOIN ledgers l ON l.id = o.party_ledger_id AND l.company_id = o.company_id
             JOIN stock_items s ON s.id = p.item_id AND s.company_id = o.company_id
             WHERE o.company_id = ?1 AND o.closed_at IS NULL AND p.quantity - p.fulfilled != p.preclosed
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
                row.get::<_, i64>(10)?,
            ))
        })?;
        let mut schedule = Vec::new();
        for row in rows {
            let (
                order_id,
                kind,
                reference,
                party,
                date,
                item_id,
                item,
                unit,
                ordered,
                fulfilled,
                preclosed,
            ) = row?;
            let remaining = remaining_quantity(ordered, fulfilled, preclosed)?;
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
                remaining,
            });
        }
        schedule.sort_by_key(|line| (line.expected_on.is_none(), line.expected_on));
        Ok(schedule)
    }

    pub fn create_order(&mut self, company: i64, draft: &OrderDraft) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let order = insert_order(&tx, company, draft)?;
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

    /// Amend the current agreement while preserving prior terms and the linked stock and invoices.
    pub fn amend_order(
        &mut self,
        company: i64,
        id: i64,
        draft: &OrderAmendmentDraft,
    ) -> Result<i64> {
        let reference = required(&draft.reference, "Order reference")?;
        let reason = required(&draft.reason, "Amendment reason")?;
        let reference_key = reference.to_lowercase();
        let notes = draft.notes.trim();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = order_summary(&tx, company, id)?;
        if current.closed_at.is_some() {
            return Err(Error::Validation("The entire order is closed".into()));
        }
        if current.revision != draft.base_revision {
            return Err(Error::Validation(
                "The order was amended since this draft was opened. Open the latest order to continue."
                    .into(),
            ));
        }
        let expected_on =
            expected_order_date(parse_date(&current.date)?, draft.expected_on.as_deref())?
                .map(|date| date.to_string());
        validate_order_lines(&tx, company, &draft.lines)?;
        let previous = order_lines(&tx, id)?;
        let updated: HashMap<_, _> = draft
            .lines
            .iter()
            .map(|line| (line.item_id, line))
            .collect();
        if reference == current.reference
            && expected_on == current.expected_on
            && notes == current.notes
            && previous.len() == updated.len()
            && previous
                .iter()
                .all(|line| updated.get(&line.line.item_id).copied() == Some(&line.line))
        {
            return Err(Error::Validation("Change at least one order field".into()));
        }
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM orders
             WHERE company_id = ?1 AND kind = ?2 AND party_ledger_id = ?3 AND reference_key = ?4 AND id != ?5)",
            params![company, current.kind.as_str(), current.party_ledger_id, reference_key, id],
            |r| r.get(0),
        )?;
        if duplicate {
            return Err(Error::Validation(
                "An order with this type, party and reference already exists".into(),
            ));
        }
        tx.execute(
            "INSERT INTO order_revisions(order_id, reference, expected_on, notes, reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                current.reference,
                current.expected_on,
                current.notes,
                reason
            ],
        )?;
        let revision = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO order_revision_lines(revision_id, item_id, item, unit, quantity, unit_price)
             SELECT ?1, ol.item_id, s.name, s.unit, ol.quantity, ol.unit_price
             FROM order_lines ol JOIN stock_items s ON s.id = ol.item_id
             WHERE ol.order_id = ?2 ORDER BY ol.id",
            params![revision, id],
        )?;
        for old in &previous {
            if let Some(line) = updated.get(&old.line.item_id) {
                if remaining_quantity(line.quantity, old.fulfilled, old.preclosed).is_err() {
                    return Err(Error::Validation(format!(
                        "Quantity for {} must cover fulfilled and pre-closed units",
                        old.item
                    )));
                }
            } else if !remove_unused_line(&tx, id, old.line.item_id)? {
                return Err(Error::Validation(format!(
                    "{} has fulfilment, closure, or reservation history. Keep it on the order and amend its quantity.",
                    old.item
                )));
            }
        }
        tx.execute(
            "UPDATE orders SET reference = ?1, reference_key = ?2, expected_on = ?3, notes = ?4
             WHERE id = ?5",
            params![reference, reference_key, expected_on, notes, id],
        )?;
        for line in &draft.lines {
            tx.execute(
                "INSERT INTO order_lines(order_id, item_id, quantity, unit_price) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(order_id, item_id) DO UPDATE
                    SET quantity = excluded.quantity, unit_price = excluded.unit_price",
                params![id, line.item_id, line.quantity, line.unit_price],
            )?;
        }
        crate::stock_reservations::check_order_coverage(&tx, id)?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn order_revisions(&self, company: i64, order: i64) -> Result<Vec<OrderRevision>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT r.id, r.reference, r.expected_on, r.notes, r.amended_at, r.reason
             FROM order_revisions r JOIN orders o ON o.id = r.order_id
             WHERE o.company_id = ?1 AND o.id = ?2 ORDER BY r.id DESC",
        )?;
        let rows = stmt.query_map(params![company, order], |r| {
            Ok(OrderRevision {
                id: r.get(0)?,
                reference: r.get(1)?,
                expected_on: r.get(2)?,
                notes: r.get(3)?,
                amended_at: r.get(4)?,
                reason: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn order_revision_lines(
        &self,
        company: i64,
        order: i64,
        revision: i64,
    ) -> Result<Vec<OrderRevisionLine>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT l.item_id, l.item, l.unit, l.quantity, l.unit_price
             FROM order_revision_lines l JOIN order_revisions r ON r.id = l.revision_id
             JOIN orders o ON o.id = r.order_id
             WHERE o.company_id = ?1 AND o.id = ?2 AND r.id = ?3 ORDER BY l.rowid",
        )?;
        let rows = stmt.query_map(params![company, order, revision], |r| {
            Ok(OrderRevisionLine {
                values: OrderLine {
                    item_id: r.get(0)?,
                    quantity: r.get(3)?,
                    unit_price: r.get(4)?,
                },
                item: r.get(1)?,
                unit: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn order_detail(&self, company: i64, id: i64) -> Result<OrderDetail> {
        let tx = self.conn.unchecked_transaction()?;
        let summary = order_summary(&tx, company, id)?;
        let lines = order_lines(&tx, id)?;
        let invoices = {
            let mut stmt = tx.prepare(
                "SELECT i.id, v.date, v.reference, rv.date FROM invoice_orders link
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
        let line_closures = {
            let mut stmt = tx.prepare(
                "SELECT c.id, s.name, s.unit, c.quantity, c.closed_on, c.reason, c.reopened_on, c.reopen_reason
                 FROM order_line_closures c JOIN order_lines ol ON ol.id = c.order_line_id
                 JOIN stock_items s ON s.id = ol.item_id WHERE ol.order_id = ?1
                 ORDER BY c.closed_on DESC, c.id DESC",
            )?;
            let rows = stmt.query_map([id], |row| {
                Ok(OrderLineClosure {
                    id: row.get(0)?,
                    item: row.get(1)?,
                    unit: row.get(2)?,
                    quantity: row.get(3)?,
                    closed_on: row.get(4)?,
                    reason: row.get(5)?,
                    reopened_on: row.get(6)?,
                    reopen_reason: row.get(7)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for row in &lines {
            total += i128::from(line_amount(row.line.quantity, row.line.unit_price)?);
        }
        let reservations = crate::stock_reservations::order_reservations(&tx, company, id)?;
        tx.commit()?;
        Ok(OrderDetail {
            summary,
            lines,
            invoices,
            line_closures,
            reservations,
            total,
        })
    }

    /// Release part or all of an item line's remaining quantity.
    pub fn preclose_order_line(
        &mut self,
        company: i64,
        order: i64,
        item: i64,
        date: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reason = required(reason, "Reason for closing the order quantity")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::advances::check_open_date(&tx, company, date)?;
        let summary = order_summary(&tx, company, order)?;
        if !summary.status.has_pending() {
            return Err(Error::Validation(
                "This order has no unfulfilled quantities".into(),
            ));
        }
        check_order_quantities(&tx, &summary, date, [(item, quantity)])?;
        check_quantity_precision(&tx, company, item, quantity)?;
        tx.execute(
            "INSERT INTO order_line_closures(order_line_id, quantity, closed_on, reason)
             SELECT id, ?3, ?4, ?5 FROM order_lines WHERE order_id = ?1 AND item_id = ?2",
            params![order, item, quantity, date.to_string(), reason],
        )?;
        let closure = tx.last_insert_rowid();
        crate::stock_reservations::check_order_coverage(&tx, order)?;
        tx.commit()?;
        Ok(closure)
    }

    pub fn reopen_order_line(
        &mut self,
        company: i64,
        order: i64,
        closure: i64,
        date: &str,
        reason: &str,
    ) -> Result<()> {
        let date = parse_date(date)?;
        let reason = required(reason, "Reason for reopening the order quantity")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::advances::check_open_date(&tx, company, date)?;
        if order_summary(&tx, company, order)?.closed_at.is_some() {
            return Err(Error::Validation("The entire order is closed".into()));
        }
        let (closed_on, reopened_on): (String, Option<String>) = tx
            .query_row(
                "SELECT c.closed_on, c.reopened_on FROM order_line_closures c
                 JOIN order_lines ol ON ol.id = c.order_line_id JOIN orders o ON o.id = ol.order_id
                 WHERE o.company_id = ?1 AND o.id = ?2 AND c.id = ?3",
                params![company, order, closure],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Closure not found on this order".into()))?;
        if reopened_on.is_some() {
            return Err(Error::Validation(
                "This quantity has already been reopened".into(),
            ));
        }
        if date < parse_date(&closed_on)? {
            return Err(Error::Validation(
                "Reopening cannot precede the closure date".into(),
            ));
        }
        tx.execute(
            "UPDATE order_line_closures SET reopened_on = ?1, reopen_reason = ?2 WHERE id = ?3",
            params![date.to_string(), reason, closure],
        )?;
        tx.commit()?;
        Ok(())
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
        check_fulfilment(
            &tx,
            company,
            id,
            draft.kind,
            draft.party_ledger_id,
            parse_date(&draft.date)?,
            FulfilmentChange::NewDocument,
            draft.lines.iter().map(|line| (line.item_id, line.quantity)),
        )?;
        let (invoice, _) =
            insert_direct_invoice(&tx, company, draft, StockInvoicePosting::Order(id))?;
        tx.execute(
            "INSERT INTO invoice_order_links(invoice_id, order_id) VALUES (?1, ?2)",
            params![invoice, id],
        )?;
        crate::stock_reservations::check_order_coverage(&tx, id)?;
        tx.commit()?;
        Ok(invoice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::stock_reservations::StockReservationDraft;

    #[test]
    fn amendments_keep_history_and_respect_fulfilment_and_reservations() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let other_company = store.create_company("Other").unwrap();
        let party = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let remove = store.create_stock_item(company, "Packing", "Each").unwrap();
        let add = store.create_stock_item(company, "Insert", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let date = "2026-01-01";
        store
            .adjust_stock_batch(company, item, location, None, date, 10_000, "Opening")
            .unwrap();
        store
            .adjust_stock_batch(company, add, location, None, date, 1000, "Opening")
            .unwrap();
        let initial = OrderDraft {
            kind: InvoiceKind::Sale,
            party_ledger_id: party,
            reference: "SO-01".into(),
            date: date.into(),
            expected_on: Some("2026-02-01".into()),
            notes: "Initial terms".into(),
            lines: vec![
                OrderLine {
                    item_id: item,
                    quantity: 10_000,
                    unit_price: 2500,
                },
                OrderLine {
                    item_id: remove,
                    quantity: 1000,
                    unit_price: 200,
                },
            ],
        };
        let order = store.create_order(company, &initial).unwrap();
        let mut other_order = initial.clone();
        other_order.reference = "SO-02".into();
        store.create_order(company, &other_order).unwrap();
        let mut delivery = StockNoteDraft {
            kind: InvoiceKind::Sale,
            date: date.into(),
            reference: "DEL-01".into(),
            party_ledger_id: party,
            order_id: Some(order),
            narration: String::new(),
            lines: vec![StockNoteLineDraft {
                item_id: item,
                location_id: location,
                batch_id: None,
                quantity: 2000,
                unit_price: 2500,
            }],
        };
        let note = store.post_stock_note(company, &delivery).unwrap();
        store
            .preclose_order_line(company, order, item, date, 1000, "No longer needed")
            .unwrap();
        let reservation = store
            .reserve_order_stock(
                company,
                order,
                &StockReservationDraft {
                    item_id: item,
                    location_id: location,
                    batch_id: None,
                    quantity: 4000,
                    reason: "Set aside".into(),
                },
            )
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE bill_note_allocations;
                 DROP TABLE bill_split_note_credits;
                 DROP TABLE bill_split_credit_allocations;
                 DROP INDEX credit_notes_with_invoice;
                 DROP INDEX credit_allocations_with_invoice;
                 DROP TABLE bill_split_advances;
                 DROP INDEX advance_allocations_with_invoice;
                 DROP TABLE bill_split_settlements;
                 DROP TABLE invoice_bill_split_revisions;
                 DROP TABLE invoice_bill_splits;
                 DROP INDEX invoice_settlements_with_invoice;
                 ALTER TABLE invoices DROP COLUMN entry_mode;
                 ALTER TABLE invoice_revisions DROP COLUMN entry_mode;
                 ALTER TABLE invoices DROP COLUMN bill_reference;
                 ALTER TABLE invoice_revisions DROP COLUMN bill_reference;
                 ALTER TABLE postings DROP COLUMN is_party;
                 ALTER TABLE revision_postings DROP COLUMN is_party;
                 ALTER TABLE recurring_journal_lines DROP COLUMN is_party;
                 DROP TABLE quotation_revisions;
                 DROP VIEW active_stock_reservation_balances;
                 DROP TABLE stock_reservation_expiries;
                 DROP VIEW warehouse_stock_movements;
             DROP TABLE negative_stock_policy_changes;
             DROP VIEW stock_transfer_location_movements;
             DROP TABLE stock_transfer_arrivals;
             DROP TABLE stock_transfer_dispatches;
             DROP TABLE order_revision_lines;
             DROP TABLE order_revisions;
             PRAGMA user_version = 66;",
            )
            .unwrap();
        let mut store = Store::from_connection(store.conn).unwrap();
        assert!(store.order_revisions(company, order).unwrap().is_empty());
        let mut amendment = OrderAmendmentDraft {
            base_revision: None,
            reference: initial.reference.clone(),
            expected_on: initial.expected_on.clone(),
            notes: initial.notes.clone(),
            lines: initial.lines.clone(),
            reason: "Change terms after shipment".into(),
        };
        assert!(store.amend_order(company, order, &amendment).is_err());
        amendment.reference = "so-02".into();
        assert!(store.amend_order(company, order, &amendment).is_err());
        amendment.reference = "SO-01A".into();
        assert!(store.amend_order(other_company, order, &amendment).is_err());
        amendment.lines[0].quantity = 2000;
        assert!(store.amend_order(company, order, &amendment).is_err());
        amendment.lines[0].quantity = 6000;
        assert!(store.amend_order(company, order, &amendment).is_err());
        assert_eq!(
            store
                .order_detail(company, order)
                .unwrap()
                .summary
                .reference,
            "SO-01"
        );
        assert!(store.order_revisions(company, order).unwrap().is_empty());

        amendment.expected_on = Some("2026-03-01".into());
        amendment.notes = "New delivery date".into();
        amendment.lines[0].quantity = 7000;
        amendment.lines[0].unit_price = 2700;
        amendment.lines[1] = OrderLine {
            item_id: add,
            quantity: 3000,
            unit_price: 100,
        };
        let revision = store.amend_order(company, order, &amendment).unwrap();
        let saved = store.order_detail(company, order).unwrap();
        assert_eq!(saved.summary.revision, Some(revision));
        assert_eq!(saved.summary.reference, "SO-01A");
        assert_eq!(saved.lines[0].unfulfilled(), 4000);
        assert_eq!(saved.lines[1].line.item_id, add);
        assert_eq!(saved.reservations[0].id, reservation);
        assert_eq!(saved.reservations[0].remaining, 4000);
        assert_eq!(
            store.stock_note_lines(company, note).unwrap()[0]
                .values
                .unit_price,
            2500
        );
        let report = store
            .pending_order_schedule(company, Some(InvoiceKind::Sale), Some("2026-03-01"))
            .unwrap();
        let amended_line = report
            .iter()
            .find(|row| row.order_id == order && row.item_id == item)
            .unwrap();
        assert_eq!(
            (amended_line.reference.as_str(), amended_line.remaining),
            ("SO-01A", 4000)
        );
        let history = store.order_revisions(company, order).unwrap();
        assert_eq!(history[0].reference, initial.reference);
        assert_eq!(history[0].expected_on, initial.expected_on);
        assert_eq!(history[0].notes, initial.notes);
        assert_eq!(history[0].reason, amendment.reason);
        assert!(store
            .order_revision_lines(other_company, order, revision)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .order_revision_lines(company, order, revision)
                .unwrap()
                .iter()
                .map(|row| row.values.clone())
                .collect::<Vec<_>>(),
            initial.lines
        );

        amendment.notes = "Add instructions".into();
        assert!(
            store.amend_order(company, order, &amendment).is_err(),
            "stale draft"
        );
        amendment.base_revision = Some(revision);
        delivery.reference = "DEL-02".into();
        delivery.lines[0].item_id = add;
        delivery.lines[0].quantity = 1000;
        let second_note = store.post_stock_note(company, &delivery).unwrap();
        store
            .reverse_stock_note(company, second_note, date, "Goods returned")
            .unwrap();
        let mut remove_used = amendment.clone();
        remove_used.lines.pop();
        assert!(store.amend_order(company, order, &remove_used).is_err());
        let second_revision = store.amend_order(company, order, &amendment).unwrap();
        let history = store.order_revisions(company, order).unwrap();
        assert_eq!(
            history.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![second_revision, revision]
        );
        store
            .close_order(company, order, "Cancelled balance")
            .unwrap();
        amendment.base_revision = Some(second_revision);
        amendment.notes = "Change closed order".into();
        assert!(store.amend_order(company, order, &amendment).is_err());
    }

    #[test]
    fn line_closures_adjust_commitments_and_keep_history_per_order() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let other = store.create_company("Other").unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let part = store.create_stock_item(company, "Part", "Each").unwrap();
        let box_id = store.create_stock_item(company, "Box", "Each").unwrap();
        let order = store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Purchase,
                    party_ledger_id: supplier,
                    reference: "PO-01".into(),
                    date: "2026-01-01".into(),
                    expected_on: Some("2026-01-31".into()),
                    notes: String::new(),
                    lines: vec![
                        OrderLine {
                            item_id: part,
                            quantity: 4000,
                            unit_price: 1500,
                        },
                        OrderLine {
                            item_id: box_id,
                            quantity: 2000,
                            unit_price: 200,
                        },
                    ],
                },
            )
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE bill_note_allocations;
                 DROP TABLE bill_split_note_credits;
                 DROP TABLE bill_split_credit_allocations;
                 DROP INDEX credit_notes_with_invoice;
                 DROP INDEX credit_allocations_with_invoice;
                 DROP TABLE bill_split_advances;
                 DROP INDEX advance_allocations_with_invoice;
                 DROP TABLE bill_split_settlements;
                 DROP TABLE invoice_bill_split_revisions;
                 DROP TABLE invoice_bill_splits;
                 DROP INDEX invoice_settlements_with_invoice;
                 ALTER TABLE invoices DROP COLUMN entry_mode;
                 ALTER TABLE invoice_revisions DROP COLUMN entry_mode;
                 ALTER TABLE invoices DROP COLUMN bill_reference;
                 ALTER TABLE invoice_revisions DROP COLUMN bill_reference;
                 ALTER TABLE postings DROP COLUMN is_party;
                 ALTER TABLE revision_postings DROP COLUMN is_party;
                 ALTER TABLE recurring_journal_lines DROP COLUMN is_party;
                 DROP TABLE quotation_revisions;
                 DROP VIEW active_stock_reservation_balances;
                 DROP TABLE stock_reservation_expiries;
                 DROP VIEW warehouse_stock_movements;
                 DROP TABLE negative_stock_policy_changes;
                 DROP VIEW stock_transfer_location_movements;
                 DROP TABLE stock_transfer_arrivals;
                 DROP TABLE stock_transfer_dispatches;
                 DROP TABLE order_revision_lines;
                 DROP TABLE order_revisions;
                 DROP VIEW stock_reservation_balances;
                 DROP TABLE stock_reservation_releases;
                 DROP TABLE stock_reservations;
                 DROP VIEW order_line_availability;
                 DROP TABLE order_line_closures;
                 PRAGMA user_version = 64;",
            )
            .unwrap();
        let mut store = Store::from_connection(store.conn).unwrap();
        let detail = store.order_detail(company, order).unwrap();
        assert!(detail.line_closures.is_empty());
        assert_eq!(detail.lines[0].unfulfilled(), 4000);
        assert!(store
            .preclose_order_line(company, order, part, "2025-12-31", 1000, "Too early")
            .is_err());
        assert!(store
            .preclose_order_line(other, order, part, "2026-01-02", 1000, "Wrong company")
            .is_err());
        assert!(store
            .preclose_order_line(company, order, part, "2026-01-02", 5000, "Too many")
            .is_err());

        let closure = store
            .preclose_order_line(company, order, part, "2026-01-02", 1000, "Reduce quantity")
            .unwrap();
        store
            .set_period_lock(company, Some("2026-01-02"), "Month closed")
            .unwrap();
        assert!(store
            .reopen_order_line(company, order, closure, "2026-01-02", "Locked date")
            .is_err());
        assert!(store
            .preclose_order_line(company, order, box_id, "2026-01-02", 1000, "Locked date")
            .is_err());
        store
            .preclose_order_line(company, order, part, "2026-01-03", 3000, "Close balance")
            .unwrap();
        let detail = store.order_detail(company, order).unwrap();
        assert_eq!(detail.summary.status, OrderStatus::Open);
        assert_eq!(detail.lines[0].preclosed, 4000);
        assert_eq!(detail.lines[0].unfulfilled(), 0);
        assert_eq!(detail.lines[1].unfulfilled(), 2000);
        assert_eq!(detail.total, 6400);
        let pending = store
            .pending_order_schedule(company, Some(InvoiceKind::Purchase), Some("2026-01-31"))
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].item_id, pending[0].remaining), (box_id, 2000));

        store
            .preclose_order_line(
                company,
                order,
                box_id,
                "2026-01-04",
                2000,
                "Close other item",
            )
            .unwrap();
        assert_eq!(
            store.order_detail(company, order).unwrap().summary.status,
            OrderStatus::Closed
        );
        assert!(store
            .reopen_order_line(other, order, closure, "2026-01-05", "Wrong company")
            .is_err());
        store
            .reopen_order_line(company, order, closure, "2026-01-05", "Need one unit")
            .unwrap();
        let detail = store.order_detail(company, order).unwrap();
        assert_eq!(detail.summary.status, OrderStatus::Open);
        let history = detail
            .line_closures
            .iter()
            .find(|line| line.id == closure)
            .unwrap();
        assert_eq!(history.reason, "Reduce quantity");
        assert_eq!(history.reopened_on.as_deref(), Some("2026-01-05"));
        assert_eq!(history.reopen_reason.as_deref(), Some("Need one unit"));
        assert!(store
            .reopen_order_line(company, order, closure, "2026-01-05", "Repeated")
            .is_err());
        let available = store.order_availability(company).unwrap();
        let part_balance = available.iter().find(|row| row.item.id == part).unwrap();
        assert_eq!(
            (part_balance.on_hand, part_balance.open_purchases),
            (0, 1000)
        );
    }
}
