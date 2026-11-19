//! Delivery and receipt notes separate stock movement from invoice posting.

use crate::inventory::{
    check_quantity_precision, check_stock_batch, check_stock_location,
    insert_invoice_on_connection, ledger_category, line_amount, InvoiceDraft, InvoiceKind,
};
use crate::{parse_date, required, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE stock_notes(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            kind TEXT NOT NULL CHECK(kind IN ('Sale', 'Purchase')),
            party_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            order_id INTEGER REFERENCES orders(id),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            date TEXT NOT NULL,
            narration TEXT NOT NULL,
            reversed_on TEXT,
            reversal_reason TEXT,
            CHECK((reversed_on IS NULL) = (reversal_reason IS NULL)),
            UNIQUE(company_id, kind, party_ledger_id, reference_key)
        );
        CREATE INDEX stock_notes_company_date ON stock_notes(company_id, date, id);
        CREATE INDEX stock_notes_order ON stock_notes(order_id);
        CREATE TABLE stock_note_lines(
            id INTEGER PRIMARY KEY,
            note_id INTEGER NOT NULL REFERENCES stock_notes(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            location_id INTEGER NOT NULL REFERENCES stock_locations(id),
            batch_id INTEGER REFERENCES stock_batches(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            unit_price INTEGER NOT NULL CHECK(unit_price >= 0),
            adjustment_id INTEGER NOT NULL UNIQUE REFERENCES stock_adjustments(id),
            reversal_adjustment_id INTEGER UNIQUE REFERENCES stock_adjustments(id)
        );
        CREATE INDEX stock_note_lines_note ON stock_note_lines(note_id, id);
        CREATE TABLE stock_note_invoices(
            invoice_id INTEGER PRIMARY KEY REFERENCES invoices(id),
            note_id INTEGER NOT NULL REFERENCES stock_notes(id)
        );
        CREATE INDEX stock_note_invoices_note ON stock_note_invoices(note_id, invoice_id);
        CREATE TABLE stock_note_invoice_lines(
            invoice_line_id INTEGER PRIMARY KEY REFERENCES invoice_lines(id),
            note_line_id INTEGER NOT NULL REFERENCES stock_note_lines(id)
        );
        CREATE VIEW physical_invoice_lines AS
            SELECT l.* FROM invoice_lines l
            WHERE NOT EXISTS(SELECT 1 FROM stock_note_invoices n WHERE n.invoice_id = l.invoice_id);
        DROP VIEW order_line_progress;
        CREATE VIEW order_line_progress AS
            SELECT ol.id, ol.order_id, ol.item_id, ol.quantity, ol.unit_price,
                COALESCE((
                    SELECT SUM(il.quantity)
                    FROM invoice_order_links link JOIN physical_invoice_lines il ON il.invoice_id = link.invoice_id
                    WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                        AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                ), 0) + COALESCE((
                    SELECT SUM(nl.quantity)
                    FROM stock_notes n JOIN stock_note_lines nl ON nl.note_id = n.id
                    WHERE n.order_id = ol.order_id AND nl.item_id = ol.item_id AND n.reversed_on IS NULL
                ), 0) AS fulfilled,
                COALESCE((
                    SELECT SUM(il.quantity)
                    FROM invoice_order_links link JOIN invoice_lines il ON il.invoice_id = link.invoice_id
                    WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                        AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                ), 0) AS billed
            FROM order_lines ol;",
    )?;
    Ok(())
}

pub(crate) fn migrate_partial_invoicing(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE INDEX stock_note_invoice_lines_note ON stock_note_invoice_lines(note_line_id);
         CREATE TABLE stock_note_invoice_spans(
            invoice_line_id INTEGER NOT NULL REFERENCES stock_note_invoice_lines(invoice_line_id),
            start_quantity INTEGER NOT NULL CHECK(start_quantity >= 0),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            PRIMARY KEY(invoice_line_id, start_quantity)
         );
         INSERT INTO stock_note_invoice_spans(invoice_line_id, start_quantity, quantity)
         SELECT link.invoice_line_id, 0, l.quantity
         FROM stock_note_invoice_lines link JOIN invoice_lines l ON l.id = link.invoice_line_id;
         CREATE VIEW stock_note_billed_quantities AS
         SELECT n.id AS note_line_id, COALESCE((
            SELECT SUM(l.quantity)
            FROM stock_note_invoice_lines link JOIN invoice_lines l ON l.id = link.invoice_line_id
            WHERE link.note_line_id = n.id
              AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = l.invoice_id)
         ), 0) AS billed
         FROM stock_note_lines n;",
    )?;
    Ok(())
}

pub fn note_label(kind: InvoiceKind) -> &'static str {
    match kind {
        InvoiceKind::Sale => "Delivery note",
        InvoiceKind::Purchase => "Receipt note",
    }
}

pub(crate) fn migrate_combined_billing(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DROP VIEW order_line_progress;
        DROP VIEW physical_invoice_lines;
        CREATE TABLE combined_stock_note_invoices(
            invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            note_id INTEGER NOT NULL REFERENCES stock_notes(id),
            PRIMARY KEY(invoice_id, note_id)
        );
        INSERT INTO combined_stock_note_invoices SELECT invoice_id, note_id FROM stock_note_invoices;
        DROP TABLE stock_note_invoices;
        ALTER TABLE combined_stock_note_invoices RENAME TO stock_note_invoices;
        CREATE INDEX stock_note_invoices_note ON stock_note_invoices(note_id, invoice_id);
        CREATE VIEW physical_invoice_lines AS
            SELECT l.* FROM invoice_lines l
            WHERE NOT EXISTS(SELECT 1 FROM stock_note_invoices n WHERE n.invoice_id = l.invoice_id);
        CREATE VIEW invoice_orders AS
            SELECT invoice_id, order_id FROM invoice_order_links
            UNION
            SELECT link.invoice_id, n.order_id FROM stock_note_invoices link
            JOIN stock_notes n ON n.id = link.note_id WHERE n.order_id IS NOT NULL;
        CREATE VIEW order_line_progress AS
            SELECT ol.id, ol.order_id, ol.item_id, ol.quantity, ol.unit_price,
                COALESCE((
                    SELECT SUM(il.quantity - COALESCE(rq.returned, 0))
                    FROM invoice_order_links link JOIN physical_invoice_lines il ON il.invoice_id = link.invoice_id
                    LEFT JOIN invoice_returned_quantities rq ON rq.line_id = il.id
                    WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                      AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                ), 0) + COALESCE((
                    SELECT SUM(nl.quantity - COALESCE(r.returned, 0) - COALESCE((
                        SELECT SUM(rq.returned) FROM invoice_returned_quantities rq WHERE rq.delivery_line = nl.id
                    ), 0))
                    FROM stock_notes n JOIN stock_note_lines nl ON nl.note_id = n.id
                    LEFT JOIN stock_note_returned_quantities r ON r.note_line_id = nl.id
                    WHERE n.order_id = ol.order_id AND nl.item_id = ol.item_id AND n.reversed_on IS NULL
                ), 0) AS fulfilled,
                COALESCE((
                    SELECT SUM(il.quantity - COALESCE(rq.returned, 0))
                    FROM invoice_order_links link JOIN physical_invoice_lines il ON il.invoice_id = link.invoice_id
                    LEFT JOIN invoice_returned_quantities rq ON rq.line_id = il.id
                    WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                      AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                ), 0) + COALESCE((
                    SELECT SUM(il.quantity - COALESCE(rq.returned, 0))
                    FROM stock_notes n JOIN stock_note_lines nl ON nl.note_id = n.id
                    JOIN stock_note_invoice_lines link ON link.note_line_id = nl.id
                    JOIN invoice_lines il ON il.id = link.invoice_line_id
                    LEFT JOIN invoice_returned_quantities rq ON rq.line_id = il.id
                    WHERE n.order_id = ol.order_id AND nl.item_id = ol.item_id
                      AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = il.invoice_id)
                ), 0) AS billed
            FROM order_lines ol;",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct StockNoteLineDraft {
    pub item_id: i64,
    pub location_id: i64,
    pub batch_id: Option<i64>,
    pub quantity: i64,
    pub unit_price: i64,
}

impl StockNoteLineDraft {
    fn bucket(&self) -> (i64, i64, Option<i64>) {
        (self.item_id, self.location_id, self.batch_id)
    }
}

#[derive(Debug, Clone)]
pub struct StockNoteDraft {
    pub kind: InvoiceKind,
    pub date: String,
    pub reference: String,
    pub party_ledger_id: i64,
    pub order_id: Option<i64>,
    pub narration: String,
    pub lines: Vec<StockNoteLineDraft>,
}

#[derive(Debug, Clone)]
pub struct StockNote {
    pub id: i64,
    pub kind: InvoiceKind,
    pub date: String,
    pub reference: String,
    pub party: String,
    pub party_ledger_id: i64,
    pub order_id: Option<i64>,
    pub narration: String,
    pub has_active_invoices: bool,
    pub has_active_returns: bool,
    pub has_unbilled_stock: bool,
    pub reversed_on: Option<String>,
    pub reversal_reason: Option<String>,
}

impl StockNote {
    pub fn status(&self) -> &'static str {
        if self.reversed_on.is_some() {
            "Reversed"
        } else if !self.has_unbilled_stock && self.has_active_returns {
            if self.has_active_invoices {
                "Invoiced and returned"
            } else {
                "Returned"
            }
        } else if !self.has_unbilled_stock {
            "Invoiced"
        } else if self.has_active_invoices {
            "Partially invoiced"
        } else if self.has_active_returns {
            "Partially returned"
        } else {
            "Awaiting invoice"
        }
    }
}

#[derive(Debug, Clone)]
pub struct StockNoteLine {
    pub id: i64,
    pub values: StockNoteLineDraft,
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub billed: i64,
    pub returned: i64,
    pub returned_after_invoice: i64,
}

impl StockNoteLine {
    pub fn unbilled(&self) -> i64 {
        self.values.quantity - self.billed - self.returned
    }
}

#[derive(Debug, Clone)]
pub struct StockNoteInvoice {
    pub invoice_id: i64,
    pub reference: String,
    pub date: String,
    pub reversed_on: Option<String>,
}

const NOTES: &str =
    "SELECT n.id, n.kind, n.date, n.reference, p.name, n.party_ledger_id,
            n.order_id, n.narration, n.reversed_on, n.reversal_reason,
            EXISTS(SELECT 1 FROM stock_note_invoices link
             WHERE link.note_id = n.id
               AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)),
            EXISTS(SELECT 1 FROM stock_note_lines l
                   LEFT JOIN stock_note_billed_quantities b ON b.note_line_id = l.id
                   LEFT JOIN stock_note_returned_quantities ret ON ret.note_line_id = l.id
                   WHERE l.note_id = n.id AND l.quantity > COALESCE(b.billed, 0) + COALESCE(ret.returned, 0)),
            (EXISTS(SELECT 1 FROM stock_returns ret WHERE ret.note_id = n.id AND ret.reversed_on IS NULL)
             OR EXISTS(SELECT 1 FROM stock_note_lines src JOIN invoice_returned_quantities ret
                       ON ret.delivery_line = src.id WHERE src.note_id = n.id))
     FROM stock_notes n JOIN ledgers p ON p.id = n.party_ledger_id AND p.company_id = n.company_id
     WHERE n.company_id = ?1";

fn read_note(row: &rusqlite::Row<'_>) -> Result<StockNote> {
    Ok(StockNote {
        id: row.get(0)?,
        kind: InvoiceKind::parse(&row.get::<_, String>(1)?)?,
        date: row.get(2)?,
        reference: row.get(3)?,
        party: row.get(4)?,
        party_ledger_id: row.get(5)?,
        order_id: row.get(6)?,
        narration: row.get(7)?,
        reversed_on: row.get(8)?,
        reversal_reason: row.get(9)?,
        has_active_invoices: row.get(10)?,
        has_unbilled_stock: row.get(11)?,
        has_active_returns: row.get(12)?,
    })
}

pub(crate) fn get_note(conn: &Connection, company: i64, note: i64) -> Result<StockNote> {
    let mut stmt = conn.prepare(&format!("{NOTES} AND n.id = ?2"))?;
    let mut rows = stmt.query(params![company, note])?;
    let row = rows
        .next()?
        .ok_or_else(|| Error::Validation("Stock note not found in the selected company".into()))?;
    read_note(row)
}

pub(crate) fn note_lines(conn: &Connection, company: i64, note: i64) -> Result<Vec<StockNoteLine>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.item_id, l.location_id, l.batch_id, l.quantity, l.unit_price,
                i.name, i.unit, loc.name, b.name, COALESCE(q.billed, 0), COALESCE(ret.returned, 0),
                COALESCE((SELECT SUM(rq.returned) FROM invoice_returned_quantities rq WHERE rq.delivery_line = l.id), 0)
         FROM stock_note_lines l JOIN stock_notes n ON n.id = l.note_id
         JOIN stock_items i ON i.id = l.item_id AND i.company_id = n.company_id
         JOIN stock_locations loc ON loc.id = l.location_id AND loc.company_id = n.company_id
         LEFT JOIN stock_batches b ON b.id = l.batch_id
         LEFT JOIN stock_note_billed_quantities q ON q.note_line_id = l.id
         LEFT JOIN stock_note_returned_quantities ret ON ret.note_line_id = l.id
         WHERE n.company_id = ?1 AND n.id = ?2 ORDER BY l.id",
    )?;
    let rows = stmt
        .query_map(params![company, note], |row| {
            Ok(StockNoteLine {
                id: row.get(0)?,
                values: StockNoteLineDraft {
                    item_id: row.get(1)?,
                    location_id: row.get(2)?,
                    batch_id: row.get(3)?,
                    quantity: row.get(4)?,
                    unit_price: row.get(5)?,
                },
                item: row.get(6)?,
                unit: row.get(7)?,
                location: row.get(8)?,
                batch: row.get(9)?,
                billed: row.get(10)?,
                returned: row.get(11)?,
                returned_after_invoice: row.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn invoice_history(conn: &Connection, company: i64, note: i64) -> Result<Vec<StockNoteInvoice>> {
    let mut stmt = conn.prepare(
        "SELECT i.id, v.reference, v.date, rv.date
         FROM stock_note_invoices link JOIN stock_notes n ON n.id = link.note_id
         JOIN invoices i ON i.id = link.invoice_id AND i.company_id = n.company_id
         JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = n.company_id
         LEFT JOIN invoice_reversals r ON r.invoice_id = i.id
         LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
         WHERE n.company_id = ?1 AND n.id = ?2
         ORDER BY i.id",
    )?;
    let rows = stmt
        .query_map(params![company, note], |row| {
            Ok(StockNoteInvoice {
                invoice_id: row.get(0)?,
                reference: row.get(1)?,
                date: row.get(2)?,
                reversed_on: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn check_after_invoice_reversals(
    conn: &Connection,
    company: i64,
    note: i64,
    date: NaiveDate,
    allow_active: bool,
) -> Result<()> {
    for invoice in invoice_history(conn, company, note)? {
        if allow_active && invoice.reversed_on.is_none() {
            continue;
        }
        let reversed = invoice.reversed_on.ok_or_else(|| {
            Error::Validation("Reverse the active invoice before changing this stock note".into())
        })?;
        if date < parse_date(&reversed)? {
            return Err(Error::Validation(
                "The date cannot precede an invoice reversal for this stock note".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn allocate_spans(
    conn: &Connection,
    note_line: &StockNoteLine,
    quantity: i64,
) -> Result<Vec<Range<i64>>> {
    let capacity = note_line.values.quantity;
    let mut stmt = conn.prepare(
        "SELECT s.start_quantity, s.quantity
         FROM stock_note_invoice_lines link
         JOIN stock_note_invoice_spans s ON s.invoice_line_id = link.invoice_line_id
         JOIN invoice_lines l ON l.id = link.invoice_line_id
         WHERE link.note_line_id = ?1
           AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = l.invoice_id)
         UNION ALL
         SELECT s.start_quantity, s.quantity FROM stock_return_lines l
         JOIN stock_returns r ON r.id = l.return_id AND r.reversed_on IS NULL
         JOIN stock_return_spans s ON s.return_line_id = l.id WHERE l.note_line_id = ?1
         ORDER BY start_quantity",
    )?;
    let rows = stmt.query_map([note_line.id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    reserve_spans(
        capacity,
        quantity,
        note_line.billed + note_line.returned,
        rows,
    )
}

pub(crate) fn reserve_spans(
    capacity: i64,
    quantity: i64,
    occupied: i64,
    rows: impl IntoIterator<Item = rusqlite::Result<(i64, i64)>>,
) -> Result<Vec<Range<i64>>> {
    let mut spans = Vec::new();
    let mut remaining = quantity;
    let mut cursor = 0;
    let mut already_allocated = 0;
    for row in rows {
        let (start, used) = row?;
        let end = start.checked_add(used).ok_or_else(invalid_spans)?;
        if start < cursor || used <= 0 || end > capacity {
            return Err(invalid_spans());
        }
        let available = (start - cursor).min(remaining);
        if available > 0 {
            spans.push(cursor..cursor + available);
            remaining -= available;
        }
        cursor = end;
        already_allocated += used;
    }
    if already_allocated != occupied {
        return Err(invalid_spans());
    }
    if remaining > capacity - cursor {
        return Err(Error::Validation(
            "Quantity exceeds the remaining amount on the source stock line".into(),
        ));
    }
    if remaining > 0 {
        spans.push(cursor..cursor + remaining);
    }
    Ok(spans)
}

#[derive(Debug, Clone)]
pub(crate) struct InvoiceQuantityShare {
    quantity: i64,
    spans: Vec<Range<i64>>,
}

impl InvoiceQuantityShare {
    pub(crate) fn new(quantity: i64, spans: Vec<Range<i64>>) -> Self {
        Self { quantity, spans }
    }

    pub(crate) fn spans(&self) -> &[Range<i64>] {
        &self.spans
    }

    /// Map spans relative to an invoice line onto the delivery that supplied it.
    pub(crate) fn project(&self, share: &Self) -> Result<Self> {
        let mut offset = 0_i64;
        let mut projected = Vec::new();
        for base in &self.spans {
            let next = offset + (base.end - base.start);
            for target in &share.spans {
                let start = target.start.max(offset);
                let end = target.end.min(next);
                if start < end {
                    projected.push(base.start + (start - offset)..base.start + (end - offset));
                }
            }
            offset = next;
        }
        if offset != share.quantity {
            return Err(invalid_spans());
        }
        Ok(Self::new(self.quantity, projected))
    }

    pub(crate) fn quantity(&self) -> i64 {
        self.quantity
    }

    /// Allocate a non-negative cost across the quantities billed.
    pub(crate) fn apportion(&self, total: i128) -> i128 {
        let quantity = i128::from(self.quantity);
        let boundary = |position: i64| {
            let position = i128::from(position);
            let scaled_remainder = (total % quantity) * position;
            let remainder = scaled_remainder % quantity;
            // Differences of rounded endpoints conserve the full cost.
            (total / quantity) * position
                + scaled_remainder / quantity
                + i128::from(remainder >= quantity - remainder)
        };
        self.spans
            .iter()
            .map(|span| boundary(span.end) - boundary(span.start))
            .sum()
    }
}

pub(crate) fn invoice_quantity_share(
    conn: &Connection,
    invoice_line: i64,
    note_quantity: i64,
    invoice_quantity: i64,
) -> Result<InvoiceQuantityShare> {
    let mut stmt = conn.prepare_cached(
        "SELECT start_quantity, quantity FROM stock_note_invoice_spans
         WHERE invoice_line_id = ?1 ORDER BY start_quantity",
    )?;
    read_quantity_share(&mut stmt, invoice_line, note_quantity, invoice_quantity)
}

pub(crate) fn read_quantity_share(
    stmt: &mut rusqlite::Statement<'_>,
    source_id: i64,
    note_quantity: i64,
    allocated_quantity: i64,
) -> Result<InvoiceQuantityShare> {
    let mut spans = Vec::new();
    let mut previous_end = 0;
    let mut subtotal = 0;
    for row in stmt.query_map([source_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (start, quantity) = row?;
        let end = start.checked_add(quantity).ok_or_else(invalid_spans)?;
        if start < previous_end || quantity <= 0 || end > note_quantity {
            return Err(invalid_spans());
        }
        spans.push(start..end);
        previous_end = end;
        subtotal += quantity;
    }
    if subtotal != allocated_quantity || spans.is_empty() {
        return Err(invalid_spans());
    }
    Ok(InvoiceQuantityShare {
        quantity: note_quantity,
        spans,
    })
}

pub(crate) fn invalid_spans() -> Error {
    Error::Validation("Stock note quantities do not reconcile".into())
}

pub(crate) fn linked_note(conn: &Connection, invoice: i64) -> Result<Option<i64>> {
    Ok(linked_notes(conn, invoice)?.first().copied())
}

pub(crate) fn linked_notes(conn: &Connection, invoice: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT note_id FROM stock_note_invoices WHERE invoice_id = ?1 ORDER BY note_id",
    )?;
    let rows = stmt
        .query_map([invoice], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn insert_movement(
    conn: &Connection,
    company: i64,
    date: NaiveDate,
    line: &StockNoteLineDraft,
    direction: i64,
    reason: &str,
) -> Result<i64> {
    crate::inventory::record_stock_adjustment(
        conn,
        company,
        line.item_id,
        line.location_id,
        line.batch_id,
        &date.to_string(),
        direction * line.quantity,
        reason,
    )
}

pub(crate) fn stock_direction(kind: InvoiceKind) -> i64 {
    match kind {
        InvoiceKind::Sale => -1,
        InvoiceKind::Purchase => 1,
    }
}

impl Store {
    pub fn stock_notes(&self, company: i64, order: Option<i64>) -> Result<Vec<StockNote>> {
        let mut stmt = self.conn.prepare(&format!(
            "{NOTES} AND (?2 IS NULL OR n.order_id = ?2) ORDER BY n.date DESC, n.id DESC"
        ))?;
        let rows = stmt.query_and_then(params![company, order], read_note)?;
        rows.collect()
    }

    pub fn stock_note_lines(&self, company: i64, note: i64) -> Result<Vec<StockNoteLine>> {
        note_lines(&self.conn, company, note)
    }

    pub fn stock_note_invoices(&self, company: i64, note: i64) -> Result<Vec<StockNoteInvoice>> {
        invoice_history(&self.conn, company, note)
    }

    /// Record a physical delivery or receipt and, optionally, consume order quantities.
    pub fn post_stock_note(&mut self, company: i64, draft: &StockNoteDraft) -> Result<i64> {
        let date = parse_date(&draft.date)?;
        let reference = required(&draft.reference, "Note reference")?;
        if draft.lines.is_empty() {
            return Err(Error::Validation("Add at least one stock line".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !matches!(
            ledger_category(&tx, company, draft.party_ledger_id)?,
            Category::Asset | Category::Liability
        ) {
            return Err(Error::Validation(
                "The party must be an asset or liability ledger".into(),
            ));
        }
        if let Some(order) = draft.order_id {
            crate::orders::check_fulfilment(
                &tx,
                company,
                order,
                draft.kind,
                draft.party_ledger_id,
                date,
                crate::orders::FulfilmentChange::NewDocument,
                draft.lines.iter().map(|line| (line.item_id, line.quantity)),
            )?;
        }
        let mut buckets = HashSet::new();
        for line in &draft.lines {
            if !buckets.insert(line.bucket()) {
                return Err(Error::Validation(
                    "Combine lines for the same item, location and batch".into(),
                ));
            }
            line_amount(line.quantity, line.unit_price)?;
            check_quantity_precision(&tx, company, line.item_id, line.quantity)?;
            check_stock_location(&tx, company, line.location_id)?;
            check_stock_batch(&tx, company, line.item_id, line.batch_id)?;
        }
        let inserted = tx.execute(
            "INSERT INTO stock_notes(company_id, kind, party_ledger_id, order_id, reference,
                 reference_key, date, narration)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(company_id, kind, party_ledger_id, reference_key) DO NOTHING",
            params![
                company,
                draft.kind.as_str(),
                draft.party_ledger_id,
                draft.order_id,
                reference,
                reference.to_lowercase(),
                date.to_string(),
                draft.narration.trim()
            ],
        )?;
        if inserted != 1 {
            return Err(Error::Validation(
                "A note with this type, party and reference already exists".into(),
            ));
        }

        let note = tx.last_insert_rowid();
        let reason = format!("{} {} (#{note})", note_label(draft.kind), reference);
        for line in &draft.lines {
            if let Some(order) = draft.order_id.filter(|_| draft.kind == InvoiceKind::Sale) {
                crate::stock_reservations::consume_reservations(
                    &tx,
                    order,
                    line.item_id,
                    line.location_id,
                    line.batch_id,
                    line.quantity,
                    &reason,
                )?;
            }
            let movement = insert_movement(
                &tx,
                company,
                date,
                line,
                stock_direction(draft.kind),
                &reason,
            )?;
            tx.execute(
                "INSERT INTO stock_note_lines(note_id, item_id, location_id, batch_id, quantity, unit_price, adjustment_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![note, line.item_id, line.location_id, line.batch_id, line.quantity, line.unit_price, movement],
            )?;
            if draft.kind == InvoiceKind::Purchase {
                crate::actual_cost::insert_receipt_cost(
                    &tx,
                    movement,
                    line_amount(line.quantity, line.unit_price)?,
                    &reason,
                )?;
            }
        }
        if let Some(order) = draft.order_id {
            crate::stock_reservations::check_order_coverage(&tx, order)?;
        }
        tx.commit()?;
        Ok(note)
    }

    pub fn reverse_stock_note(
        &mut self,
        company: i64,
        id: i64,
        date: &str,
        reason: &str,
    ) -> Result<()> {
        let date = parse_date(date)?;
        let reason = required(reason, "Stock reversal reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = get_note(&tx, company, id)?;
        if note.reversed_on.is_some() {
            return Err(Error::Validation(
                "This stock note has already been reversed".into(),
            ));
        }
        if date < parse_date(&note.date)? {
            return Err(Error::Validation(
                "Reversal date cannot precede the stock note date".into(),
            ));
        }
        check_after_invoice_reversals(&tx, company, id, date, false)?;
        crate::stock_returns::check_after_returns(&tx, company, id, date, false)?;
        let movement_reason = format!(
            "{} {} (#{id}) reversal: {reason}",
            note_label(note.kind),
            note.reference
        );
        let lines = note_lines(&tx, company, id)?;
        if lines.is_empty() {
            return Err(Error::Validation(
                "This stock note has no stock lines".into(),
            ));
        }
        for line in lines {
            let movement = insert_movement(
                &tx,
                company,
                date,
                &line.values,
                -stock_direction(note.kind),
                &movement_reason,
            )?;
            tx.execute(
                "UPDATE stock_note_lines SET reversal_adjustment_id = ?1 WHERE id = ?2",
                params![movement, line.id],
            )?;
        }
        tx.execute(
            "UPDATE stock_notes SET reversed_on = ?1, reversal_reason = ?2 WHERE id = ?3",
            params![date.to_string(), reason, id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Bill unbilled quantities from one note, matched by item, location and batch.
    pub fn invoice_stock_note(
        &mut self,
        company: i64,
        id: i64,
        draft: &InvoiceDraft,
    ) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lines = note_lines(&tx, company, id)?;
        let expected: HashMap<_, _> = lines
            .iter()
            .map(|line| (line.values.bucket(), line.id))
            .collect();
        let sources = draft
            .lines
            .iter()
            .map(|line| {
                expected
                    .get(&(line.item_id, line.location_id, line.batch_id))
                    .copied()
                    .ok_or_else(|| {
                        Error::Validation("Use lines from the selected stock note".into())
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let invoice = bill_stock_notes(&tx, company, &sources, draft)?;
        tx.commit()?;
        Ok(invoice)
    }

    /// Bill one or more notes for the same party and type. IDs correspond to the
    /// draft lines in order, and identify the stock note line for each quantity.
    pub fn invoice_stock_notes(
        &mut self,
        company: i64,
        sources: &[i64],
        draft: &InvoiceDraft,
    ) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let invoice = bill_stock_notes(&tx, company, sources, draft)?;
        tx.commit()?;
        Ok(invoice)
    }
}

fn bill_stock_notes(
    conn: &Connection,
    company: i64,
    sources: &[i64],
    draft: &InvoiceDraft,
) -> Result<i64> {
    if sources.is_empty() || sources.len() != draft.lines.len() {
        return Err(Error::Validation(
            "Select the source stock note line for every invoice line".into(),
        ));
    }
    let date = parse_date(&draft.date)?;
    let mut note_ids = std::collections::BTreeSet::new();
    let mut source_lines = HashMap::new();
    let mut plan = Vec::with_capacity(sources.len());
    let mut lookup = conn.prepare_cached("SELECT note_id FROM stock_note_lines WHERE id = ?1")?;
    for (&source, line) in sources.iter().zip(&draft.lines) {
        let id: i64 = lookup
            .query_row([source], |row| row.get(0))
            .optional()?
            .ok_or_else(|| Error::Validation("A source stock note line was not found".into()))?;
        if note_ids.insert(id) {
            let note = get_note(conn, company, id)?;
            if note.reversed_on.is_some() || !note.has_unbilled_stock {
                return Err(Error::Validation(
                    "Select stock notes with quantities available to invoice".into(),
                ));
            }
            if date < parse_date(&note.date)? {
                return Err(Error::Validation(
                    "Invoice date cannot precede any source stock note date".into(),
                ));
            }
            if draft.kind != note.kind || draft.party_ledger_id != note.party_ledger_id {
                return Err(Error::Validation(
                    "All source stock notes must match the invoice type and party".into(),
                ));
            }
            check_after_invoice_reversals(conn, company, id, date, true)?;
            crate::stock_returns::check_after_returns(conn, company, id, date, true)?;
            source_lines.extend(
                note_lines(conn, company, id)?
                    .into_iter()
                    .map(|line| (line.id, line)),
            );
        }
        let note_line = source_lines.remove(&source).ok_or_else(|| {
            Error::Validation("Use each source stock note line once per invoice".into())
        })?;
        if line.quantity <= 0
            || !line.extra_allocations.is_empty()
            || (line.item_id, line.location_id, line.batch_id) != note_line.values.bucket()
        {
            return Err(Error::Validation("Use a positive quantity and the original item, location and batch on each stock note line".into()));
        }
        plan.push((source, allocate_spans(conn, &note_line, line.quantity)?));
    }

    let (invoice, _) = insert_invoice_on_connection(conn, company, draft)?;
    for id in note_ids {
        conn.execute(
            "INSERT INTO stock_note_invoices(invoice_id, note_id) VALUES (?1, ?2)",
            params![invoice, id],
        )?;
    }
    let mut stmt =
        conn.prepare("SELECT id FROM invoice_lines WHERE invoice_id = ?1 ORDER BY id")?;
    let invoice_lines = stmt
        .query_map([invoice], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if invoice_lines.len() != plan.len() {
        return Err(invalid_spans());
    }
    for (invoice_line, (note_line, spans)) in invoice_lines.into_iter().zip(plan) {
        conn.execute(
            "INSERT INTO stock_note_invoice_lines(invoice_line_id, note_line_id) VALUES (?1, ?2)",
            params![invoice_line, note_line],
        )?;
        for span in spans {
            conn.execute(
                "INSERT INTO stock_note_invoice_spans(invoice_line_id, start_quantity, quantity) VALUES (?1, ?2, ?3)",
                params![invoice_line, span.start, span.end - span.start],
            )?;
        }
    }
    Ok(invoice)
}

#[cfg(test)]
mod combined_billing_tests {
    use super::{StockNoteDraft, StockNoteLineDraft};
    use crate::actual_cost::ActualCostMethod;
    use crate::inventory::{InvoiceDiscount, InvoiceDraft, InvoiceKind, InvoiceLine};
    use crate::invoice_returns::ReturnLineDraft;
    use crate::orders::{OrderDraft, OrderLine};
    use crate::{Category, Store};

    #[test]
    fn one_invoice_keeps_quantities_and_costs_with_each_note_and_order() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Demo").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let sales = store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let line = |quantity, unit_price| StockNoteLineDraft {
            item_id: item,
            location_id: location,
            batch_id: None,
            quantity,
            unit_price,
        };
        for (reference, quantity, cost) in [("REC-A", 2000, 500), ("REC-B", 3000, 700)] {
            store
                .post_stock_note(
                    company,
                    &StockNoteDraft {
                        kind: InvoiceKind::Purchase,
                        date: "2026-04-01".into(),
                        reference: reference.into(),
                        party_ledger_id: supplier,
                        order_id: None,
                        narration: String::new(),
                        lines: vec![line(quantity, cost)],
                    },
                )
                .unwrap();
        }
        let mut orders = Vec::new();
        let mut notes = Vec::new();
        let mut sources = Vec::new();
        for (reference, date, quantity) in [("A", "2026-04-02", 2000), ("B", "2026-04-03", 3000)] {
            let order = store
                .create_order(
                    company,
                    &OrderDraft {
                        kind: InvoiceKind::Sale,
                        party_ledger_id: customer,
                        reference: format!("SO-{reference}"),
                        date: "2026-04-01".into(),
                        expected_on: None,
                        notes: String::new(),
                        lines: vec![OrderLine {
                            item_id: item,
                            quantity,
                            unit_price: 1000,
                        }],
                    },
                )
                .unwrap();
            let note = store
                .post_stock_note(
                    company,
                    &StockNoteDraft {
                        kind: InvoiceKind::Sale,
                        date: date.into(),
                        reference: format!("DEL-{reference}"),
                        party_ledger_id: customer,
                        order_id: Some(order),
                        narration: String::new(),
                        lines: vec![line(quantity, 1000)],
                    },
                )
                .unwrap();
            orders.push(order);
            notes.push(note);
            sources.push(store.stock_note_lines(company, note).unwrap()[0].id);
        }
        let mut draft = InvoiceDraft {
            kind: InvoiceKind::Sale,
            entry_mode: crate::inventory::InvoiceEntryMode::Invoice,
            date: "2026-04-02".into(),
            due_date: None,
            credit_days: None,
            reference: "INV-01".into(),
            bill_reference: None,
            narration: String::new(),
            party_ledger_id: customer,
            trade_ledger_id: sales,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: vec![],
            tax_calculation: None,
            adjustments: vec![],
            lines: [1000, 3000]
                .into_iter()
                .map(|quantity| InvoiceLine {
                    item_id: item,
                    location_id: location,
                    batch_id: None,
                    trade_ledger_id: None,
                    extra_allocations: vec![],
                    quantity,
                    unit_price: 1000,
                    discount: InvoiceDiscount::None,
                })
                .collect(),
        };
        assert!(store
            .invoice_stock_notes(company, &sources, &draft)
            .is_err());
        draft.date = "2026-04-04".into();
        assert!(store
            .invoice_stock_notes(company, &[sources[0], sources[0]], &draft)
            .is_err());
        assert!(store.invoices(company).unwrap().is_empty());
        let invoice = store
            .invoice_stock_notes(company, &sources, &draft)
            .unwrap();
        let posted = &store.invoices(company).unwrap()[0];
        assert_eq!(posted.stock_note_ids, notes);
        assert_eq!(posted.order_ids, orders);
        for (index, billed) in [1000, 3000].into_iter().enumerate() {
            let detail = store.order_detail(company, orders[index]).unwrap();
            assert_eq!(detail.lines[0].billed, billed);
            assert_eq!(detail.invoices[0].id, invoice);
            assert_eq!(
                store.stock_note_invoices(company, notes[index]).unwrap()[0].invoice_id,
                invoice
            );
            assert!(store
                .reverse_stock_note(company, notes[index], "2026-04-04", "Already billed")
                .is_err());
        }
        let margin = store
            .sales_at_actual_cost(company, None, Some("2026-04-04"), ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(
            (margin.rows.len(), margin.net_sales, margin.cost_of_sales),
            (2, 4000, 2600)
        );
        let allocations = store.invoice_return_allocations(company, invoice).unwrap();
        let credit = store
            .post_invoice_stock_return(
                company,
                invoice,
                "2026-04-05",
                "RET-01",
                "Two items returned",
                &[
                    ReturnLineDraft {
                        allocation: allocations[0].id,
                        quantity: 1000,
                    },
                    ReturnLineDraft {
                        allocation: allocations[1].id,
                        quantity: 1000,
                    },
                ],
                &[],
            )
            .unwrap();
        assert_eq!(
            store.order_detail(company, orders[0]).unwrap().lines[0].billed,
            0
        );
        assert_eq!(
            store.order_detail(company, orders[1]).unwrap().lines[0].billed,
            2000
        );
        store
            .reverse_invoice_credit(company, credit, "2026-04-06", "RET-REV", "Reaccepted parts")
            .unwrap();
        assert_eq!(
            store.order_detail(company, orders[0]).unwrap().lines[0].billed,
            1000
        );
        assert_eq!(
            store.order_detail(company, orders[1]).unwrap().lines[0].billed,
            3000
        );
        store
            .reverse_invoice(company, invoice, "2026-04-07", "INV-REV", "Replace invoice")
            .unwrap();
        assert_eq!(
            store.stock_note_lines(company, notes[0]).unwrap()[0].unbilled(),
            2000
        );
        assert_eq!(
            store.stock_note_lines(company, notes[1]).unwrap()[0].unbilled(),
            3000
        );
        assert_eq!(store.stock_balances(company, None).unwrap()[0].quantity, 0);
    }
}
