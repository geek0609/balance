//! Delivery and receipt notes separate stock movement from invoice posting.

use crate::inventory::{
    check_quantity_precision, check_stock_batch, check_stock_location,
    insert_invoice_on_connection, ledger_category, line_amount, InvoiceDraft, InvoiceKind,
};
use crate::{parse_date, required, Category, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{HashMap, HashSet};

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
    pub active_invoice_id: Option<i64>,
    pub reversed_on: Option<String>,
    pub reversal_reason: Option<String>,
}

impl StockNote {
    pub fn status(&self) -> &'static str {
        if self.reversed_on.is_some() {
            "Reversed"
        } else if self.active_invoice_id.is_some() {
            "Invoiced"
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
            (SELECT link.invoice_id FROM stock_note_invoices link
             WHERE link.note_id = n.id
               AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id))
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
        active_invoice_id: row.get(10)?,
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

fn note_lines(conn: &Connection, company: i64, note: i64) -> Result<Vec<StockNoteLine>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.item_id, l.location_id, l.batch_id, l.quantity, l.unit_price,
                i.name, i.unit, loc.name, b.name
         FROM stock_note_lines l JOIN stock_notes n ON n.id = l.note_id
         JOIN stock_items i ON i.id = l.item_id AND i.company_id = n.company_id
         JOIN stock_locations loc ON loc.id = l.location_id AND loc.company_id = n.company_id
         LEFT JOIN stock_batches b ON b.id = l.batch_id
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

fn check_after_invoice_reversals(
    conn: &Connection,
    company: i64,
    note: i64,
    date: NaiveDate,
) -> Result<()> {
    for invoice in invoice_history(conn, company, note)? {
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

pub(crate) fn linked_note(conn: &Connection, invoice: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT note_id FROM stock_note_invoices WHERE invoice_id = ?1",
            [invoice],
            |row| row.get(0),
        )
        .optional()?)
}

fn insert_movement(
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

fn stock_direction(kind: InvoiceKind) -> i64 {
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
        check_after_invoice_reversals(&tx, company, id, date)?;
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

    /// Bill the full physical note once. Its stock remains at the original movement date.
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
        if note.reversed_on.is_some() || note.active_invoice_id.is_some() {
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
        check_after_invoice_reversals(&tx, company, id, date)?;

        let lines = note_lines(&tx, company, id)?;
        let mut expected: HashMap<_, _> = lines
            .iter()
            .map(|line| (line.values.bucket(), line.values.quantity))
            .collect();
        if lines.is_empty() || lines.len() != draft.lines.len() {
            return Err(Error::Validation(
                "Invoice all stock note lines together".into(),
            ));
        }
        for line in &draft.lines {
            let key = (line.item_id, line.location_id, line.batch_id);
            if !line.extra_allocations.is_empty() || expected.remove(&key) != Some(line.quantity) {
                return Err(Error::Validation(
                    "Invoice items, quantities, locations and batches must match the stock note"
                        .into(),
                ));
            }
        }

        let (invoice, _) = insert_invoice_on_connection(&tx, company, draft)?;
        tx.execute(
            "INSERT INTO stock_note_invoices(invoice_id, note_id) VALUES (?1, ?2)",
            params![invoice, id],
        )?;
        let linked = tx.execute(
            "INSERT INTO stock_note_invoice_lines(invoice_line_id, note_line_id)
             SELECT l.id, nl.id FROM invoice_lines l JOIN stock_note_lines nl
                 ON l.item_id = nl.item_id AND l.location_id = nl.location_id AND l.batch_id IS nl.batch_id
             WHERE l.invoice_id = ?1 AND nl.note_id = ?2",
            params![invoice, id],
        )?;
        if linked != lines.len() {
            return Err(Error::Validation(
                "Invoice lines could not be linked to the stock note".into(),
            ));
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
