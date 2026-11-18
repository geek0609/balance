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
            EXISTS(SELECT 1 FROM stock_returns ret WHERE ret.note_id = n.id AND ret.reversed_on IS NULL)
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
                i.name, i.unit, loc.name, b.name, COALESCE(q.billed, 0), COALESCE(ret.returned, 0)
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
    if already_allocated != note_line.billed + note_line.returned {
        return Err(invalid_spans());
    }
    if remaining > capacity - cursor {
        return Err(Error::Validation(format!(
            "Quantity exceeds the unbilled amount for {}",
            note_line.item
        )));
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
    Ok(conn
        .query_row(
            "SELECT note_id FROM stock_note_invoices WHERE invoice_id = ?1",
            [invoice],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn insert_movement(
    conn: &Connection,
    company: i64,
    date: NaiveDate,
    line: &StockNoteLineDraft,
    direction: i64,
    reason: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO stock_adjustments(company_id, item_id, date, quantity, reason, location_id, batch_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![company, line.item_id, date.to_string(), direction * line.quantity, reason,
            line.location_id, line.batch_id],
    )?;
    Ok(conn.last_insert_rowid())
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

    /// Bill unbilled quantities from a note. Stock stays on the movement date.
    pub fn invoice_stock_note(
        &mut self,
        company: i64,
        id: i64,
        draft: &InvoiceDraft,
    ) -> Result<i64> {
        let date = parse_date(&draft.date)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = get_note(&tx, company, id)?;
        if note.reversed_on.is_some() || !note.has_unbilled_stock {
            return Err(Error::Validation(
                "Select a stock note awaiting invoice".into(),
            ));
        }
        if date < parse_date(&note.date)? {
            return Err(Error::Validation(
                "Invoice date cannot precede the stock note date".into(),
            ));
        }
        if draft.kind != note.kind || draft.party_ledger_id != note.party_ledger_id {
            return Err(Error::Validation(
                "Invoice type and party must match the stock note".into(),
            ));
        }
        check_after_invoice_reversals(&tx, company, id, date, true)?;
        crate::stock_returns::check_after_returns(&tx, company, id, date, true)?;

        let lines = note_lines(&tx, company, id)?;
        let mut expected: HashMap<_, _> = lines
            .iter()
            .map(|line| (line.values.bucket(), line))
            .collect();
        if draft.lines.is_empty() {
            return Err(Error::Validation(
                "Include at least one unbilled stock note line".into(),
            ));
        }
        let mut plan = Vec::with_capacity(draft.lines.len());
        for line in &draft.lines {
            let key = (line.item_id, line.location_id, line.batch_id);
            if !line.extra_allocations.is_empty() || line.quantity <= 0 {
                return Err(Error::Validation(
                    "Each stock note line needs a positive quantity at its recorded location and batch".into(),
                ));
            }
            let note_line = expected.remove(&key).ok_or_else(|| {
                Error::Validation(
                    "Use each stock note item, location and batch at most once per invoice".into(),
                )
            })?;
            plan.push((note_line.id, allocate_spans(&tx, note_line, line.quantity)?));
        }

        let (invoice, _) = insert_invoice_on_connection(&tx, company, draft)?;
        tx.execute(
            "INSERT INTO stock_note_invoices(invoice_id, note_id) VALUES (?1, ?2)",
            params![invoice, id],
        )?;
        {
            let mut stmt =
                tx.prepare("SELECT id FROM invoice_lines WHERE invoice_id = ?1 ORDER BY id")?;
            let invoice_lines = stmt
                .query_map([invoice], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if invoice_lines.len() != plan.len() {
                return Err(invalid_spans());
            }
            for (invoice_line, (note_line, spans)) in invoice_lines.into_iter().zip(plan) {
                tx.execute(
                    "INSERT INTO stock_note_invoice_lines(invoice_line_id, note_line_id) VALUES (?1, ?2)",
                    params![invoice_line, note_line],
                )?;
                for span in spans {
                    tx.execute(
                        "INSERT INTO stock_note_invoice_spans(invoice_line_id, start_quantity, quantity)
                         VALUES (?1, ?2, ?3)",
                        params![invoice_line, span.start, span.end - span.start],
                    )?;
                }
            }
        }
        if let Some(order) = note.order_id {
            tx.execute(
                "INSERT INTO invoice_order_links(invoice_id, order_id) VALUES (?1, ?2)",
                params![invoice, order],
            )?;
        }
        tx.commit()?;
        Ok(invoice)
    }
}
