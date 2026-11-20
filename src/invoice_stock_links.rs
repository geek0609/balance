//! Match separately recorded deliveries and receipts to items billed in advance.
use crate::inventory::check_quantity_precision;
use crate::stock_notes::{self, InvoiceQuantityShare, StockNote, StockNoteLine};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS invoice_stock_links(
            id INTEGER PRIMARY KEY,
            invoice_allocation_id INTEGER NOT NULL REFERENCES invoice_line_allocations(id),
            note_line_id INTEGER NOT NULL REFERENCES stock_note_lines(id),
            linked_on TEXT NOT NULL,
            billed_start INTEGER NOT NULL CHECK(billed_start >= 0),
            note_start INTEGER NOT NULL CHECK(note_start >= 0),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            released_on TEXT CHECK(released_on IS NULL OR released_on >= linked_on),
            release_reason TEXT,
            CHECK((released_on IS NULL) = (release_reason IS NULL))
        );
        CREATE INDEX IF NOT EXISTS invoice_stock_links_invoice ON invoice_stock_links(invoice_allocation_id);
        CREATE INDEX IF NOT EXISTS invoice_stock_links_note ON invoice_stock_links(note_line_id);
        CREATE VIEW IF NOT EXISTS invoice_allocation_billing_offsets AS
            SELECT id AS allocation_id, line_id, COALESCE(billed_quantity, quantity) AS billed_quantity,
                   COALESCE(SUM(COALESCE(billed_quantity, quantity)) OVER(PARTITION BY line_id ORDER BY id
                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING), 0) AS billed_offset
            FROM invoice_line_allocations;
        CREATE VIEW IF NOT EXISTS invoice_stock_link_history AS
            SELECT s.*, a.line_id, l.invoice_id, n.id AS note_id, n.date AS note_date,
                   n.reversed_on AS note_reversed_on, rv.date AS invoice_reversed_on
            FROM invoice_stock_links s JOIN invoice_line_allocations a ON a.id = s.invoice_allocation_id
            JOIN invoice_lines l ON l.id = a.line_id
            JOIN stock_note_lines nl ON nl.id = s.note_line_id
            JOIN stock_notes n ON n.id = nl.note_id
            LEFT JOIN invoice_reversals r ON r.invoice_id = l.invoice_id
            LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id;
        CREATE VIEW IF NOT EXISTS active_invoice_stock_links AS
            SELECT * FROM invoice_stock_link_history
            WHERE invoice_reversed_on IS NULL AND note_reversed_on IS NULL AND released_on IS NULL;
        CREATE VIEW IF NOT EXISTS stock_note_billing_history AS
            SELECT invoice_id, note_id, NULL AS released_on, 0 AS releasable FROM stock_note_invoices
            UNION ALL
            SELECT invoice_id, note_id,
                   CASE WHEN COUNT(*) = COUNT(released_on) THEN MAX(released_on) END,
                   COUNT(*) > COUNT(released_on)
            FROM invoice_stock_link_history GROUP BY invoice_id, note_id;
        CREATE VIEW IF NOT EXISTS active_stock_note_invoice_spans AS
            SELECT link.note_line_id, s.start_quantity, s.quantity
            FROM stock_note_invoice_lines link
            JOIN stock_note_invoice_spans s ON s.invoice_line_id = link.invoice_line_id
            JOIN invoice_lines l ON l.id = link.invoice_line_id
            WHERE NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = l.invoice_id)
            UNION ALL
            SELECT note_line_id, note_start, quantity FROM active_invoice_stock_links;
        DROP VIEW stock_note_billed_quantities;
        CREATE VIEW stock_note_billed_quantities AS
            SELECT l.id AS note_line_id, COALESCE(SUM(s.quantity), 0) AS billed
            FROM stock_note_lines l LEFT JOIN active_stock_note_invoice_spans s ON s.note_line_id = l.id
            GROUP BY l.id;
        DROP VIEW invoice_orders;
        CREATE VIEW invoice_orders AS
            SELECT invoice_id, order_id FROM invoice_order_links
            UNION
            SELECT h.invoice_id, n.order_id FROM stock_note_billing_history h
            JOIN stock_notes n ON n.id = h.note_id WHERE n.order_id IS NOT NULL;
        DROP VIEW order_line_progress;
        CREATE VIEW order_line_progress AS
            SELECT ol.id, ol.order_id, ol.item_id, ol.quantity, ol.unit_price,
                   COALESCE(direct.quantity, 0) + COALESCE(notes.fulfilled, 0) AS fulfilled,
                   COALESCE(direct.quantity, 0) + COALESCE(notes.billed, 0) AS billed
            FROM order_lines ol
            LEFT JOIN (
                SELECT link.order_id, il.item_id, SUM(il.quantity - COALESCE(ret.returned, 0)) AS quantity
                FROM invoice_order_links link JOIN physical_invoice_lines il ON il.invoice_id = link.invoice_id
                LEFT JOIN invoice_returned_quantities ret ON ret.line_id = il.id
                WHERE NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                GROUP BY link.order_id, il.item_id
            ) direct ON direct.order_id = ol.order_id AND direct.item_id = ol.item_id
            LEFT JOIN (
                SELECT n.order_id, l.item_id,
                       SUM(l.quantity - COALESCE(ret.returned, 0) - COALESCE(ir.returned, 0)) AS fulfilled,
                       SUM(COALESCE(b.billed, 0) - COALESCE(ir.returned, 0)) AS billed
                FROM stock_notes n JOIN stock_note_lines l ON l.note_id = n.id
                LEFT JOIN stock_note_billed_quantities b ON b.note_line_id = l.id
                LEFT JOIN stock_note_returned_quantities ret ON ret.note_line_id = l.id
                LEFT JOIN (
                    SELECT delivery_line, SUM(returned) AS returned FROM invoice_returned_quantities
                    WHERE delivery_line IS NOT NULL GROUP BY delivery_line
                ) ir ON ir.delivery_line = l.id
                WHERE n.order_id IS NOT NULL AND n.reversed_on IS NULL
                GROUP BY n.order_id, l.item_id
            ) notes ON notes.order_id = ol.order_id AND notes.item_id = ol.item_id;",
    )?;
    Ok(())
}

/// The open quantity of a billed allocation on an existing invoice.
#[derive(Debug)]
pub struct InvoiceStockDemand {
    pub allocation_id: i64,
    pub item_id: i64,
    pub location_id: i64,
    pub batch_id: Option<i64>,
    pub billed_quantity: i64,
    pub remaining: i64,
}

impl InvoiceStockDemand {
    pub fn matches(&self, line: &StockNoteLine) -> bool {
        self.item_id == line.values.item_id
            && self.location_id == line.values.location_id
            && self
                .batch_id
                .is_none_or(|batch| Some(batch) == line.values.batch_id)
    }
}

#[derive(Debug)]
pub struct InvoiceAwaitingStock {
    pub id: i64,
    pub date: String,
    pub reference: String,
    pub allocations: Vec<InvoiceStockDemand>,
}

#[derive(Debug)]
pub struct StockNoteInvoiceAllocation {
    pub invoice_allocation_id: i64,
    pub note_line_id: i64,
    pub quantity: i64,
}

fn demand(conn: &Connection, company: i64, invoice: i64) -> Result<Vec<InvoiceStockDemand>> {
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, l.item_id, a.location_id, a.batch_id, a.billed_quantity,
                a.billed_quantity - COALESCE(SUM(s.quantity), 0)
         FROM invoices i JOIN invoice_lines l ON l.invoice_id = i.id
         JOIN invoice_line_allocations a ON a.line_id = l.id
         LEFT JOIN active_invoice_stock_links s ON s.invoice_allocation_id = a.id
         WHERE i.company_id = ?1 AND i.id = ?2 AND a.quantity = 0
           AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = i.id)
         GROUP BY a.id ORDER BY l.id, a.id",
    )?;
    let rows = stmt.query_map(params![company, invoice], |row| {
        Ok(InvoiceStockDemand {
            allocation_id: row.get(0)?,
            item_id: row.get(1)?,
            location_id: row.get(2)?,
            batch_id: row.get(3)?,
            billed_quantity: row.get(4)?,
            remaining: row.get(5)?,
        })
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if rows
        .iter()
        .any(|row| !(0..=row.billed_quantity).contains(&row.remaining))
    {
        return Err(stock_notes::invalid_spans());
    }
    Ok(rows)
}

fn candidates(
    conn: &Connection,
    company: i64,
    note: &StockNote,
    invoice: Option<i64>,
) -> Result<Vec<InvoiceAwaitingStock>> {
    if note.reversed_on.is_some() || !note.has_unbilled_stock {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT i.id, v.date, v.reference FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
         WHERE i.company_id = ?1 AND i.kind = ?2 AND i.party_ledger_id = ?3 AND v.date <= ?4
           AND (?7 IS NULL OR i.id = ?7)
           AND EXISTS(SELECT 1 FROM invoice_lines l JOIN invoice_line_allocations a ON a.line_id = l.id
                JOIN stock_note_lines nl ON nl.note_id = ?5 AND nl.item_id = l.item_id
                     AND nl.location_id = a.location_id AND (a.batch_id IS NULL OR a.batch_id = nl.batch_id)
                WHERE l.invoice_id = i.id AND a.quantity = 0)
           AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = i.id)
           AND NOT EXISTS(SELECT 1 FROM invoice_order_links o
                          WHERE o.invoice_id = i.id AND o.order_id IS NOT ?6)
         ORDER BY v.date DESC, i.id DESC",
    )?;
    let rows = stmt.query_map(
        params![
            company,
            note.kind.as_str(),
            note.party_ledger_id,
            note.date,
            note.id,
            note.order_id,
            invoice,
        ],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let lines = stock_notes::note_lines(conn, company, note.id)?;
    let mut choices = Vec::new();
    for row in rows {
        let (id, date, reference) = row?;
        let allocations = demand(conn, company, id)?
            .into_iter()
            .filter(|a| {
                a.remaining > 0
                    && lines
                        .iter()
                        .any(|line| line.unbilled() > 0 && a.matches(line))
            })
            .collect::<Vec<_>>();
        if !allocations.is_empty() {
            choices.push(InvoiceAwaitingStock {
                id,
                date,
                reference,
                allocations,
            });
        }
    }
    Ok(choices)
}

fn attach_spans(
    conn: &Connection,
    invoice_allocation: i64,
    note_line: i64,
    date: &str,
    invoice_spans: Vec<Range<i64>>,
    note_spans: Vec<Range<i64>>,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO invoice_stock_links(invoice_allocation_id, note_line_id, linked_on, billed_start, note_start, quantity)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut invoice = invoice_spans.into_iter().peekable();
    let mut note = note_spans.into_iter().peekable();
    while let (Some(bill), Some(stock)) = (invoice.peek_mut(), note.peek_mut()) {
        let len = (bill.end - bill.start).min(stock.end - stock.start);
        stmt.execute(params![
            invoice_allocation,
            note_line,
            date,
            bill.start,
            stock.start,
            len
        ])?;
        bill.start += len;
        stock.start += len;
        if bill.is_empty() {
            invoice.next();
        }
        if stock.is_empty() {
            note.next();
        }
    }
    if invoice.next().is_some() || note.next().is_some() {
        return Err(stock_notes::invalid_spans());
    }
    Ok(())
}

fn check_link_date(conn: &Connection, allocation: i64, note_line: i64, date: &str) -> Result<()> {
    let invalid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM invoice_stock_link_history
         WHERE (invoice_allocation_id = ?1 OR note_line_id = ?2)
           AND (linked_on > ?3 OR released_on > ?3 OR note_reversed_on > ?3 OR invoice_reversed_on > ?3))",
        params![allocation, note_line, date],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(Error::Validation(
            "Link date cannot precede a later link or reversal for these quantities".into(),
        ));
    }
    Ok(())
}

pub(crate) fn check_invoice_reversal_date(
    conn: &Connection,
    invoice: i64,
    date: NaiveDate,
) -> Result<()> {
    let invalid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM invoice_stock_link_history WHERE invoice_id = ?1 AND COALESCE(released_on, linked_on) > ?2)",
        params![invoice, date.to_string()],
        |r| r.get(0),
    )?;
    if invalid {
        return Err(Error::Validation(
            "Invoice reversal date cannot precede a linked stock allocation".into(),
        ));
    }
    Ok(())
}

pub(crate) fn deferred_quantity(conn: &Connection, invoice_line: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(billed_quantity), 0) FROM invoice_line_allocations
         WHERE line_id = ?1 AND quantity = 0",
        [invoice_line],
        |row| row.get(0),
    )?)
}

pub(crate) fn physical_net(conn: &Connection, line: i64, billed: i64, amount: i64) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "SELECT b.billed_offset, b.billed_quantity
         FROM invoice_line_allocations a JOIN invoice_allocation_billing_offsets b ON b.allocation_id = a.id
         WHERE a.line_id = ?1 AND b.line_id = ?1 AND a.quantity = 0 ORDER BY a.id",
    )?;
    let mut rows = stmt.query([line])?;
    let mut deferred = 0_i128;
    while let Some(row) = rows.next()? {
        let share = InvoiceQuantityShare::for_range(billed, row.get(0)?, row.get(1)?)?;
        deferred += share.apportion(i128::from(amount));
    }
    i64::try_from(i128::from(amount) - deferred)
        .ok()
        .filter(|&physical| physical >= 0)
        .ok_or_else(stock_notes::invalid_spans)
}

pub(crate) struct LinkedDelivery {
    pub note_line: i64,
    pub date: NaiveDate,
    pub share: InvoiceQuantityShare,
    pub quantity: i64,
}

pub(crate) fn deliveries(
    conn: &Connection,
    invoice_line: i64,
    end: Option<NaiveDate>,
) -> Result<Vec<LinkedDelivery>> {
    let mut stmt = conn.prepare_cached(
        "SELECT link.note_line_id, link.note_date, nl.quantity, link.note_start, link.quantity
         FROM invoice_stock_link_history link JOIN stock_note_lines nl ON nl.id = link.note_line_id
         WHERE link.line_id = ?1 AND (?2 IS NULL OR link.linked_on <= ?2)
           AND (link.released_on IS NULL OR (?2 IS NOT NULL AND link.released_on > ?2))
           AND (link.note_reversed_on IS NULL OR link.note_reversed_on != link.note_date)
         ORDER BY link.note_line_id, link.note_start",
    )?;
    let mut rows = stmt.query(params![invoice_line, end.map(|date| date.to_string())])?;
    let mut deliveries = Vec::new();
    while let Some(row) = rows.next()? {
        let note_units = row.get(2)?;
        let start = row.get::<_, i64>(3)?;
        let quantity = row.get::<_, i64>(4)?;
        deliveries.push(LinkedDelivery {
            note_line: row.get(0)?,
            date: parse_date(&row.get::<_, String>(1)?)?,
            share: InvoiceQuantityShare::for_range(note_units, start, quantity)?,
            quantity,
        });
    }
    Ok(deliveries)
}

impl Store {
    pub fn invoices_awaiting_stock_on_note(
        &self,
        company: i64,
        note: i64,
    ) -> Result<Vec<InvoiceAwaitingStock>> {
        let note = stock_notes::get_note(&self.conn, company, note)?;
        candidates(&self.conn, company, &note, None)
    }

    pub fn invoice_stock_demand(
        &self,
        company: i64,
        invoice: i64,
    ) -> Result<Vec<InvoiceStockDemand>> {
        demand(&self.conn, company, invoice)
    }

    /// Assign posted quantities to an invoice without posting fresh stock or ledger movements.
    pub fn link_invoice_stock_note(
        &mut self,
        company: i64,
        note: i64,
        invoice: i64,
        linked_on: &str,
        requests: &[StockNoteInvoiceAllocation],
    ) -> Result<()> {
        let date = parse_date(linked_on)?;
        if requests.is_empty() {
            return Err(Error::Validation("Enter a quantity to match".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = stock_notes::get_note(&tx, company, note)?;
        if date < parse_date(&note.date)? {
            return Err(Error::Validation(
                "Link date cannot precede the stock note date".into(),
            ));
        }
        if crate::periods::cutoff(&tx, company)?.is_some_and(|locked| date <= locked) {
            return Err(Error::Validation(
                crate::periods::LOCKED_PERIOD_MESSAGE.into(),
            ));
        }
        stock_notes::check_after_invoice_reversals(&tx, company, note.id, date, true)?;
        crate::stock_returns::check_after_returns(&tx, company, note.id, date, true)?;
        let candidate = candidates(&tx, company, &note, Some(invoice))?
            .into_iter()
            .find(|entry| entry.id == invoice)
            .ok_or_else(|| {
                Error::Validation("No billed allocations match this stock note".into())
            })?;
        let mut demands: HashMap<_, _> = candidate
            .allocations
            .into_iter()
            .map(|a| (a.allocation_id, a))
            .collect();
        let mut lines: HashMap<_, _> = stock_notes::note_lines(&tx, company, note.id)?
            .into_iter()
            .map(|l| (l.id, l))
            .collect();
        let mut used = HashSet::new();
        let date = date.to_string();
        for request in requests {
            let &StockNoteInvoiceAllocation {
                invoice_allocation_id,
                note_line_id,
                quantity,
            } = request;
            if !used.insert((invoice_allocation_id, note_line_id)) {
                return Err(Error::Validation(
                    "Combine quantities for each pair of matching allocations".into(),
                ));
            }
            let line = lines.get_mut(&note_line_id).ok_or_else(|| {
                Error::Validation("Choose a line on the selected stock note".into())
            })?;
            let billed = demands
                .get_mut(&invoice_allocation_id)
                .filter(|a| a.matches(line))
                .ok_or_else(|| {
                    Error::Validation("Choose a matching billed item and location".into())
                })?;
            if quantity <= 0 || quantity > line.unbilled() || quantity > billed.remaining {
                return Err(Error::Validation(
                    "Quantity exceeds the available stock or billed quantity".into(),
                ));
            }
            check_quantity_precision(&tx, company, billed.item_id, quantity)?;
            check_link_date(&tx, invoice_allocation_id, note_line_id, &date)?;
            let note_spans = stock_notes::allocate_spans(&tx, line, quantity)?;
            let mut stmt = tx.prepare_cached(
                "SELECT billed_start, quantity FROM active_invoice_stock_links
                 WHERE invoice_allocation_id = ?1 ORDER BY billed_start",
            )?;
            let invoice_spans = stock_notes::reserve_spans(
                billed.billed_quantity,
                quantity,
                billed.billed_quantity - billed.remaining,
                stmt.query_map([invoice_allocation_id], |r| Ok((r.get(0)?, r.get(1)?)))?,
            )?;
            attach_spans(
                &tx,
                invoice_allocation_id,
                note_line_id,
                &date,
                invoice_spans,
                note_spans,
            )?;
            billed.remaining -= quantity;
            line.billed += quantity;
        }
        tx.commit()?;
        Ok(())
    }

    /// Release a note's active allocations to this invoice on the specified date.
    pub fn release_invoice_stock_note(
        &mut self,
        company: i64,
        note: i64,
        invoice: i64,
        released_on: &str,
        reason: &str,
    ) -> Result<()> {
        let date = parse_date(released_on)?;
        let reason = required(reason, "Stock match release reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        stock_notes::get_note(&tx, company, note)?;
        if crate::periods::cutoff(&tx, company)?.is_some_and(|locked| date <= locked) {
            return Err(Error::Validation(
                crate::periods::LOCKED_PERIOD_MESSAGE.into(),
            ));
        }
        let date = date.to_string();
        {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT invoice_allocation_id, note_line_id FROM active_invoice_stock_links
                 WHERE note_id = ?1 AND invoice_id = ?2",
            )?;
            for row in
                stmt.query_map(params![note, invoice], |row| Ok((row.get(0)?, row.get(1)?)))?
            {
                let (allocation, note_line) = row?;
                check_link_date(&tx, allocation, note_line, &date)?;
            }
        }
        let count = tx.execute(
            "UPDATE invoice_stock_links SET released_on = ?3, release_reason = ?4
             WHERE id IN (SELECT id FROM active_invoice_stock_links WHERE note_id = ?1 AND invoice_id = ?2)",
            params![note, invoice, date, reason],
        )?;
        if count == 0 {
            return Err(Error::Validation(
                "No active stock match on this note and invoice".into(),
            ));
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actual_cost::{ActualCostIssue, ActualCostMethod};
    use crate::inventory::{
        InvoiceAllocation, InvoiceDiscount, InvoiceDraft, InvoiceEntryMode, InvoiceKind,
        InvoiceLine,
    };
    use crate::invoice_returns::ReturnLineDraft;
    use crate::orders::{OrderDraft, OrderLine};
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::Category;

    fn invoice(
        kind: InvoiceKind,
        party: i64,
        trade: i64,
        item: i64,
        location: i64,
    ) -> InvoiceDraft {
        InvoiceDraft {
            kind,
            entry_mode: InvoiceEntryMode::Invoice,
            date: "2026-04-01".into(),
            reference: "INVOICE".into(),
            external_reference: Default::default(),
            bill_reference: None,
            due_date: None,
            credit_days: None,
            bill_splits: Vec::new(),
            narration: String::new(),
            party_ledger_id: party,
            trade_ledger_id: trade,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: Vec::new(),
            tax_calculation: None,
            lines: vec![InvoiceLine {
                item_id: item,
                location_id: location,
                batch_id: None,
                trade_ledger_id: None,
                discount: InvoiceDiscount::None,
                extra_allocations: Vec::new(),
                quantity: 0,
                billed_quantity: Some(3000),
                unit_price: 1001,
                rate_basis: None,
            }],
            adjustments: Vec::new(),
        }
    }

    #[test]
    fn matched_deliveries_cost_a_paid_invoice_as_of_the_link_date() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Sales").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let bank = store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        let trade = store
            .create_ledger(company, "Income", Category::Income)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let opening = store
            .adjust_stock_at(company, item, location, "2026-04-01", 6000, "Opening")
            .unwrap();
        store
            .set_stock_receipt_cost(company, opening, 3600, "Cost")
            .unwrap();
        store
            .record_standard_stock_cost(company, item, "2026-04-01", 700, "Initial rate")
            .unwrap();
        store
            .record_standard_stock_cost(company, item, "2026-04-05", 800, "New rate")
            .unwrap();
        let id = store
            .create_invoice(
                company,
                &invoice(InvoiceKind::Sale, customer, trade, item, location),
            )
            .unwrap();
        store
            .settle_invoices(
                company,
                bank,
                "2026-04-02",
                "PAID",
                &[crate::inventory::SettlementAllocation {
                    invoice_id: id,
                    amount: 3003,
                }],
            )
            .unwrap();
        let mut notes = Vec::new();
        for (date, reference, quantity) in [
            ("2026-04-03", "FIRST", 1000),
            ("2026-04-05", "SECOND", 2000),
        ] {
            let note = store
                .post_stock_note(
                    company,
                    &StockNoteDraft {
                        kind: InvoiceKind::Sale,
                        date: date.into(),
                        reference: reference.into(),
                        party_ledger_id: customer,
                        order_id: None,
                        narration: String::new(),
                        lines: vec![StockNoteLineDraft {
                            item_id: item,
                            location_id: location,
                            batch_id: None,
                            quantity,
                            unit_price: 0,
                        }],
                    },
                )
                .unwrap();
            let matching = store
                .invoices_awaiting_stock_on_note(company, note)
                .unwrap();
            assert_eq!(matching[0].id, id);
            let ledger_before = store
                .ledger_register(company, customer, None, None)
                .unwrap()
                .closing_balance;
            assert_eq!(ledger_before, 0);
            let line = store.stock_note_lines(company, note).unwrap().remove(0);
            let requested = StockNoteInvoiceAllocation {
                invoice_allocation_id: matching[0].allocations[0].allocation_id,
                note_line_id: line.id,
                quantity,
            };
            let link_date = if reference == "FIRST" {
                "2026-04-04"
            } else {
                date
            };
            store
                .link_invoice_stock_note(company, note, id, link_date, &[requested])
                .unwrap();
            assert_eq!(
                store.stock_note_lines(company, note).unwrap()[0].billed,
                quantity
            );
            assert_eq!(
                store.stock_note_invoices(company, note).unwrap()[0].invoice_id,
                id
            );
            notes.push(note);
        }
        let invoice = store.invoices(company).unwrap().remove(0);
        assert_eq!((invoice.total, invoice.outstanding), (3003, 0));
        assert_eq!(invoice.stock_note_ids, notes);
        assert_eq!(invoice.lines[0].allocations[0].linked_stock_quantity, 3000);
        assert_eq!(
            store.invoice_stock_demand(company, id).unwrap()[0].remaining,
            0
        );
        let register = store.stock_register(company, item, None, None).unwrap();
        assert_eq!(
            register
                .movements
                .iter()
                .map(|row| row.quantity)
                .collect::<Vec<_>>(),
            [6000, -1000, -2000]
        );
        for (end, qty, cost) in [
            ("2026-04-03", 0, None),
            ("2026-04-04", 1000, None),
            ("2026-04-05", 3000, Some(1800)),
        ] {
            for method in ActualCostMethod::ALL {
                let report = store
                    .sales_at_actual_cost(company, None, Some(end), method)
                    .unwrap();
                let row = &report.rows[0];
                assert_eq!(
                    (report.net_sales, row.sale.quantity, row.cost),
                    (3003, qty, cost)
                );
                assert_eq!(
                    matches!(row.issue, Some(ActualCostIssue::PendingStockIssue)),
                    cost.is_none()
                );
            }
        }
        let standard = store.sales_at_standard_cost(company, None, None).unwrap();
        assert_eq!(standard.rows[0].unit_cost, None);
        assert_eq!(standard.rows[0].standard_cost, Some(2300));
        assert!(store
            .reverse_stock_note(company, notes[0], "2026-04-06", "Cancel")
            .is_err());
        assert!(store
            .release_invoice_stock_note(company, notes[0], id, "2026-04-03", "Too early")
            .is_err());
        store
            .release_invoice_stock_note(
                company,
                notes[0],
                id,
                "2026-04-06",
                "Correct delivery link",
            )
            .unwrap();
        let history = store.stock_note_invoices(company, notes[0]).unwrap();
        assert_eq!(history[0].released_on.as_deref(), Some("2026-04-06"));
        assert!(!history[0].can_release);
        assert_eq!(
            store.invoice_stock_demand(company, id).unwrap()[0].remaining,
            1000
        );
        assert_eq!(
            store.stock_note_lines(company, notes[0]).unwrap()[0].unbilled(),
            1000
        );
        assert_eq!(store.invoices(company).unwrap()[0].outstanding, 0);
        let as_of = |store: &Store, date| {
            store
                .sales_at_actual_cost(company, None, Some(date), ActualCostMethod::Fifo)
                .unwrap()
                .rows[0]
                .cost
        };
        assert_eq!(as_of(&store, "2026-04-05"), Some(1800));
        assert_eq!(as_of(&store, "2026-04-06"), None);
        let allocation = store.invoice_stock_demand(company, id).unwrap()[0].allocation_id;
        let line = store.stock_note_lines(company, notes[0]).unwrap()[0].id;
        let request = || StockNoteInvoiceAllocation {
            invoice_allocation_id: allocation,
            note_line_id: line,
            quantity: 1000,
        };
        assert!(store
            .link_invoice_stock_note(company, notes[0], id, "2026-04-05", &[request()])
            .is_err());
        store
            .link_invoice_stock_note(company, notes[0], id, "2026-04-07", &[request()])
            .unwrap();
        assert_eq!(as_of(&store, "2026-04-07"), Some(1800));
        store
            .release_invoice_stock_note(
                company,
                notes[0],
                id,
                "2026-04-08",
                "Cancel first delivery",
            )
            .unwrap();
        store
            .reverse_stock_note(company, notes[0], "2026-04-08", "Cancel first delivery")
            .unwrap();
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            4000
        );
    }

    #[test]
    fn receipt_linking_preserves_values_orders_and_reversal_history() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Purchases").unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let trade = store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let east = store.create_stock_location(company, "East").unwrap();
        let west = store.create_stock_location(company, "West").unwrap();
        let batch = store
            .create_stock_batch(company, item, "Lot", None, None)
            .unwrap();
        let mut bill = invoice(InvoiceKind::Purchase, supplier, trade, item, east);
        bill.lines[0].quantity = 1000;
        bill.lines[0].discount = InvoiceDiscount::Fixed(1);
        bill.lines[0].extra_allocations.push(InvoiceAllocation {
            location_id: west,
            batch_id: None,
            quantity: 0,
            billed_quantity: Some(2000),
        });
        let id = store.create_invoice(company, &bill).unwrap();
        assert_eq!(
            store
                .stock_valuation_at_average_cost(company, "2026-04-01")
                .unwrap()
                .known_value,
            1001
        );
        let physical = store.invoice_return_allocations(company, id).unwrap()[0].id;
        assert_eq!(
            store
                .invoice_return_subtotal(
                    company,
                    id,
                    &[ReturnLineDraft {
                        allocation: physical,
                        quantity: 1000
                    }]
                )
                .unwrap(),
            1001
        );
        let order = store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Purchase,
                    party_ledger_id: supplier,
                    reference: "ORDER".into(),
                    date: "2026-04-01".into(),
                    expected_on: None,
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: item,
                        quantity: 2000,
                        unit_price: 850,
                    }],
                },
            )
            .unwrap();
        let note = store
            .post_stock_note(
                company,
                &StockNoteDraft {
                    kind: InvoiceKind::Purchase,
                    date: "2026-04-02".into(),
                    reference: "RECEIPT".into(),
                    party_ledger_id: supplier,
                    order_id: Some(order),
                    narration: String::new(),
                    lines: vec![StockNoteLineDraft {
                        item_id: item,
                        location_id: west,
                        batch_id: Some(batch),
                        quantity: 2000,
                        unit_price: 850,
                    }],
                },
            )
            .unwrap();
        let line = store.stock_note_lines(company, note).unwrap().remove(0);
        let allocation = store.invoice_stock_demand(company, id).unwrap()[0].allocation_id;
        let request = || StockNoteInvoiceAllocation {
            invoice_allocation_id: allocation,
            note_line_id: line.id,
            quantity: 1000,
        };
        store
            .set_period_lock(company, Some("2026-04-02"), "Close period")
            .unwrap();
        assert!(store
            .link_invoice_stock_note(company, note, id, "2026-04-02", &[request()])
            .is_err());
        assert!(store
            .link_invoice_stock_note(company, note, id, "2026-04-03", &[request(), request()])
            .is_err());
        assert_eq!(store.stock_note_lines(company, note).unwrap()[0].billed, 0);
        store
            .link_invoice_stock_note(company, note, id, "2026-04-03", &[request()])
            .unwrap();
        store
            .link_invoice_stock_note(company, note, id, "2026-04-04", &[request()])
            .unwrap();
        assert!(store
            .link_invoice_stock_note(company, note, id, "2026-04-04", &[request()])
            .is_err());
        let detail = store.order_detail(company, order).unwrap();
        assert_eq!(
            (detail.lines[0].fulfilled, detail.lines[0].billed),
            (2000, 2000)
        );
        for (date, qty, amount, cost) in [
            ("2026-04-02", 0, 0, 0),
            ("2026-04-03", 1000, 1000, 850),
            ("2026-04-04", 2000, 2001, 1700),
        ] {
            let row = store
                .receipt_reconciliation(company, Some(date))
                .unwrap()
                .remove(0);
            assert_eq!(
                (
                    row.invoiced_quantity,
                    row.invoice_net,
                    row.invoiced_receipt_cost
                ),
                (qty, amount, Some(cost))
            );
        }
        assert_eq!(
            store
                .stock_valuation_at_average_cost(company, "2026-04-04")
                .unwrap()
                .known_value,
            2701
        );
        assert!(store
            .reverse_invoice(company, id, "2026-04-03", "CANCEL", "Too early")
            .is_err());
        store
            .reverse_invoice(company, id, "2026-04-05", "CANCEL", "Cancel invoice")
            .unwrap();
        assert_eq!(
            store.stock_note_lines(company, note).unwrap()[0].unbilled(),
            2000
        );
        assert_eq!(
            store.order_detail(company, order).unwrap().lines[0].billed,
            0
        );
        assert_eq!(
            store
                .receipt_reconciliation(company, Some("2026-04-04"))
                .unwrap()[0]
                .invoice_net,
            2001
        );
        assert_eq!(
            store
                .receipt_reconciliation(company, Some("2026-04-05"))
                .unwrap()[0]
                .invoice_net,
            0
        );
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            2000
        );
        store
            .reverse_stock_note(company, note, "2026-04-06", "Cancel receipt")
            .unwrap();
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            0
        );
    }
}
