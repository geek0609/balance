//! Assign order fulfilment and closures to individual delivery lots.
use crate::{Error, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::ops::Range;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS order_lot_assignments(
            id INTEGER PRIMARY KEY,
            lot_id INTEGER NOT NULL REFERENCES order_delivery_lots(id),
            invoice_allocation_id INTEGER REFERENCES invoice_line_allocations(id),
            note_line_id INTEGER REFERENCES stock_note_lines(id),
            closure_id INTEGER REFERENCES order_line_closures(id),
            start_quantity INTEGER NOT NULL CHECK(start_quantity >= 0),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            CHECK((invoice_allocation_id IS NOT NULL) + (note_line_id IS NOT NULL) + (closure_id IS NOT NULL) = 1)
         );
         CREATE INDEX IF NOT EXISTS order_lot_assignments_lot ON order_lot_assignments(lot_id);
         CREATE INDEX IF NOT EXISTS order_lot_assignments_invoice ON order_lot_assignments(invoice_allocation_id);
         CREATE INDEX IF NOT EXISTS order_lot_assignments_note ON order_lot_assignments(note_line_id);
         CREATE TABLE IF NOT EXISTS order_lot_returns(
            id INTEGER PRIMARY KEY,
            assignment_id INTEGER NOT NULL REFERENCES order_lot_assignments(id),
            invoice_return_line_id INTEGER REFERENCES invoice_return_lines(id),
            stock_return_line_id INTEGER REFERENCES stock_return_lines(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            CHECK((invoice_return_line_id IS NULL) != (stock_return_line_id IS NULL))
         );
         CREATE INDEX IF NOT EXISTS order_lot_returns_assignment ON order_lot_returns(assignment_id);
         CREATE VIEW IF NOT EXISTS active_order_lot_returns AS
            SELECT a.assignment_id, a.quantity FROM order_lot_returns a
            JOIN invoice_stock_return_history r ON r.id = a.invoice_return_line_id
            WHERE r.reversed_on IS NULL
            UNION ALL
            SELECT a.assignment_id, a.quantity FROM order_lot_returns a
            JOIN stock_return_lines l ON l.id = a.stock_return_line_id
            JOIN stock_returns r ON r.id = l.return_id WHERE r.reversed_on IS NULL;
         CREATE VIEW IF NOT EXISTS active_order_lot_assignments AS
            SELECT a.id, a.lot_id, a.closure_id,
                   a.quantity - COALESCE((SELECT SUM(r.quantity)
                        FROM active_order_lot_returns r WHERE r.assignment_id = a.id), 0) AS quantity
            FROM order_lot_assignments a
            LEFT JOIN invoice_line_allocations ia ON ia.id = a.invoice_allocation_id
            LEFT JOIN invoice_lines il ON il.id = ia.line_id
            LEFT JOIN invoice_reversals ir ON ir.invoice_id = il.invoice_id
            LEFT JOIN stock_note_lines nl ON nl.id = a.note_line_id
            LEFT JOIN stock_notes n ON n.id = nl.note_id
            LEFT JOIN order_line_closures c ON c.id = a.closure_id
            WHERE ir.invoice_id IS NULL AND n.reversed_on IS NULL AND c.reopened_on IS NULL;
         CREATE VIEW IF NOT EXISTS order_delivery_lot_balances AS
            SELECT lot.id, lot.order_line_id, lot.quantity,
                   lot.quantity - COALESCE(SUM(a.quantity), 0) AS unassigned,
                   COALESCE(SUM(a.quantity), 0) AS assigned
            FROM order_delivery_lots lot LEFT JOIN active_order_lot_assignments a ON a.lot_id = lot.id
            GROUP BY lot.id;
         DROP VIEW order_schedule_progress;
         CREATE VIEW order_schedule_progress AS
             WITH assigned_by_line AS (
                SELECT order_line_id, SUM(assigned) AS assigned
                FROM order_delivery_lot_balances GROUP BY order_line_id
             )
             SELECT id AS lot_id, line_id, order_id, item_id, expected_on, quantity,
                    MIN(unassigned, MAX(0, cumulative - fallback_used)) AS remaining
             FROM (
                SELECT lot.id, p.id AS line_id, p.order_id, p.item_id, lot.quantity,
                       balance.unassigned,
                       p.fulfilled + p.preclosed - totals.assigned AS fallback_used,
                       COALESCE(lot.expected_on, o.expected_on) AS expected_on,
                       SUM(balance.unassigned) OVER(PARTITION BY p.id
                           ORDER BY COALESCE(lot.expected_on, o.expected_on) IS NULL,
                                    COALESCE(lot.expected_on, o.expected_on), lot.id
                           ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS cumulative
                FROM order_line_availability p JOIN orders o ON o.id = p.order_id
                JOIN order_delivery_lots lot ON lot.order_line_id = p.id
                JOIN order_delivery_lot_balances balance ON balance.id = lot.id
                JOIN assigned_by_line totals ON totals.order_line_id = p.id
             )
             UNION ALL
             SELECT NULL, p.id, p.order_id, p.item_id, o.expected_on, p.quantity, p.quantity - p.fulfilled - p.preclosed
             FROM order_line_availability p JOIN orders o ON o.id = p.order_id
             WHERE NOT EXISTS(SELECT 1 FROM order_delivery_lots lot WHERE lot.order_line_id = p.id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct LotAssignment {
    lot: i64,
    span: Range<i64>,
}

struct OpenLot {
    id: Option<i64>,
    expected_on: Option<String>,
    remaining: i64,
}

/// Allocate dated requests before distributing undated quantities across remaining lots.
pub(crate) fn plan<'a>(
    conn: &Connection,
    order: i64,
    requests: impl IntoIterator<Item = (i64, i64, Option<&'a str>)>,
) -> Result<Vec<Vec<LotAssignment>>> {
    let mut requests = requests.into_iter().enumerate().collect::<Vec<_>>();
    let mut assignments = vec![Vec::new(); requests.len()];
    if requests.iter().all(|(_, (_, quantity, _))| *quantity == 0) {
        return Ok(assignments);
    }
    let mut stmt = conn.prepare_cached(
        "SELECT item_id, lot_id, expected_on, remaining FROM order_schedule_progress
         WHERE order_id = ?1 AND remaining > 0 ORDER BY expected_on IS NULL, expected_on, lot_id",
    )?;
    let mut open = HashMap::<i64, Vec<OpenLot>>::new();
    let mut rows = stmt.query([order])?;
    while let Some(row) = rows.next()? {
        open.entry(row.get(0)?).or_default().push(OpenLot {
            id: row.get(1)?,
            expected_on: row.get(2)?,
            remaining: row.get(3)?,
        });
    }
    requests.sort_by_key(|(_, (_, _, expected))| expected.is_none());
    for (index, (item, quantity, expected)) in requests {
        let mut consumed = 0;
        for lot in open.entry(item).or_default() {
            if consumed == quantity {
                break;
            }
            if expected.is_some_and(|date| Some(date) != lot.expected_on.as_deref()) {
                continue;
            }
            let amount = (quantity - consumed).min(lot.remaining);
            if amount == 0 {
                continue;
            }
            if let Some(lot) = lot.id {
                assignments[index].push(LotAssignment {
                    lot,
                    span: consumed..consumed + amount,
                });
            }
            consumed += amount;
            lot.remaining -= amount;
        }
        if consumed != quantity {
            return Err(Error::Validation(match expected {
                Some(date) => format!("Order quantity exceeds the available lot for {date}"),
                None => "Order quantity exceeds the available delivery schedule".into(),
            }));
        }
    }
    Ok(assignments)
}

#[derive(Clone, Copy)]
pub(crate) enum Source {
    InvoiceAllocation(i64),
    NoteLine(i64),
    Closure(i64),
}

impl Source {
    fn ids(self) -> (Option<i64>, Option<i64>, Option<i64>) {
        match self {
            Self::InvoiceAllocation(id) => (Some(id), None, None),
            Self::NoteLine(id) => (None, Some(id), None),
            Self::Closure(id) => (None, None, Some(id)),
        }
    }
}

pub(crate) fn write(
    conn: &Connection,
    source: Source,
    assignments: &[LotAssignment],
) -> Result<()> {
    let (invoice, note, closure) = source.ids();
    for entry in assignments {
        conn.execute(
            "INSERT INTO order_lot_assignments(lot_id, invoice_allocation_id, note_line_id, closure_id, start_quantity, quantity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![entry.lot, invoice, note, closure, entry.span.start, entry.span.end - entry.span.start],
        )?;
    }
    Ok(())
}

pub(crate) enum Return {
    Invoice(i64),
    Stock(i64),
}

pub(crate) fn record_return(
    conn: &Connection,
    source: Source,
    document: Return,
    ranges: &[Range<i64>],
) -> Result<()> {
    let (invoice, note, _) = source.ids();
    let (invoice_return, stock_return) = match document {
        Return::Invoice(id) => (Some(id), None),
        Return::Stock(id) => (None, Some(id)),
    };
    let mut stmt = conn.prepare_cached(
        "SELECT id, start_quantity, quantity FROM order_lot_assignments
         WHERE (invoice_allocation_id = ?1 OR note_line_id = ?2) ORDER BY start_quantity",
    )?;
    let rows = stmt.query_map(params![invoice, note], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (assignment, start, qty) = row?;
        let end = start
            .checked_add(qty)
            .ok_or_else(|| Error::Validation("Order lot ranges do not reconcile".into()))?;
        let quantity: i64 = ranges
            .iter()
            .map(|range| (end.min(range.end) - start.max(range.start)).max(0))
            .sum();
        if quantity > 0 {
            conn.execute(
                "INSERT INTO order_lot_returns(assignment_id, invoice_return_line_id, stock_return_line_id, quantity)
                 VALUES (?1, ?2, ?3, ?4)",
                params![assignment, invoice_return, stock_return, quantity],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn check_capacity(conn: &Connection, order: i64) -> Result<()> {
    let invalid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM order_delivery_lot_balances b
         JOIN order_lines l ON l.id = b.order_line_id
         WHERE l.order_id = ?1 AND (b.unassigned < 0 OR b.assigned < 0))",
        [order],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(Error::Validation(
            "The change would exceed an assigned delivery lot".into(),
        ));
    }
    Ok(())
}
