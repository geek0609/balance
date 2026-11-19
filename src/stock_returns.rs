//! Dated returns of goods on a delivery or receipt that have yet to be billed.

use crate::inventory::{check_quantity_precision, stock_bucket_quantity, InvoiceKind};
use crate::stock_notes::{self, InvoiceQuantityShare, StockNoteLineDraft};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE stock_returns(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            note_id INTEGER NOT NULL REFERENCES stock_notes(id),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            date TEXT NOT NULL,
            reason TEXT NOT NULL,
            reversed_on TEXT,
            reversal_reason TEXT,
            CHECK((reversed_on IS NULL) = (reversal_reason IS NULL)),
            UNIQUE(note_id, reference_key)
        );
        CREATE TABLE stock_return_lines(
            id INTEGER PRIMARY KEY,
            return_id INTEGER NOT NULL REFERENCES stock_returns(id),
            note_line_id INTEGER NOT NULL REFERENCES stock_note_lines(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            adjustment_id INTEGER NOT NULL UNIQUE REFERENCES stock_adjustments(id),
            reversal_adjustment_id INTEGER UNIQUE REFERENCES stock_adjustments(id),
            UNIQUE(return_id, note_line_id)
        );
        CREATE INDEX stock_return_lines_note ON stock_return_lines(note_line_id);
        CREATE TABLE stock_return_spans(
            return_line_id INTEGER NOT NULL REFERENCES stock_return_lines(id),
            start_quantity INTEGER NOT NULL CHECK(start_quantity >= 0),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            PRIMARY KEY(return_line_id, start_quantity)
        );
        CREATE VIEW stock_note_returned_quantities AS
            SELECT line.note_line_id, SUM(line.quantity) AS returned FROM stock_return_lines line
            JOIN stock_returns r ON r.id = line.return_id AND r.reversed_on IS NULL
            GROUP BY line.note_line_id;
        DROP VIEW order_line_progress;
        CREATE VIEW order_line_progress AS
            SELECT ol.id, ol.order_id, ol.item_id, ol.quantity, ol.unit_price,
                COALESCE((
                    SELECT SUM(il.quantity)
                    FROM invoice_order_links link JOIN physical_invoice_lines il ON il.invoice_id = link.invoice_id
                    WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                        AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                ), 0) + COALESCE((
                    SELECT SUM(nl.quantity - COALESCE(r.returned, 0))
                    FROM stock_notes n JOIN stock_note_lines nl ON nl.note_id = n.id
                    LEFT JOIN stock_note_returned_quantities r ON r.note_line_id = nl.id
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

#[derive(Debug, Clone)]
pub struct StockReturn {
    pub id: i64,
    pub reference: String,
    pub date: String,
    pub reason: String,
    pub reversed_on: Option<String>,
    pub reversal_reason: Option<String>,
    pub lines: Vec<StockReturnLine>,
}

#[derive(Debug, Clone)]
pub struct StockReturnLine {
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub quantity: i64,
}

#[derive(Debug, Clone)]
pub struct StockReturnLineDraft {
    pub note_line: i64,
    pub quantity: i64,
}

pub(crate) fn check_after_returns(
    conn: &Connection,
    company: i64,
    note: i64,
    date: NaiveDate,
    allow_active: bool,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT reversed_on FROM stock_returns WHERE company_id = ?1 AND note_id = ?2",
    )?;
    for reversal in stmt.query_map(params![company, note], |row| {
        row.get::<_, Option<String>>(0)
    })? {
        match reversal? {
            Some(reversed) if date < parse_date(&reversed)? => {
                return Err(Error::Validation(
                    "Date cannot precede a return reversal for this stock note".into(),
                ));
            }
            None if !allow_active => {
                return Err(Error::Validation(
                    "Reverse active returns before reversing this stock note".into(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn returned_on(
    conn: &Connection,
    note_line: i64,
    as_of: Option<NaiveDate>,
) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "SELECT l.quantity, r.date, r.reversed_on FROM stock_return_lines l
         JOIN stock_returns r ON r.id = l.return_id WHERE l.note_line_id = ?1",
    )?;
    let mut rows = stmt.query([note_line])?;
    let mut total = 0_i64;
    while let Some(row) = rows.next()? {
        let date = parse_date(&row.get::<_, String>(1)?)?;
        let reversal = row
            .get::<_, Option<String>>(2)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        if as_of.is_none_or(|end| date <= end)
            && reversal.is_none_or(|date| as_of.is_some_and(|end| date > end))
        {
            total = total
                .checked_add(row.get(0)?)
                .ok_or_else(stock_notes::invalid_spans)?;
        }
    }
    Ok(total)
}

pub(crate) fn cost_share(
    conn: &Connection,
    line: i64,
    capacity: i64,
    quantity: i64,
) -> Result<InvoiceQuantityShare> {
    let mut stmt = conn.prepare_cached(
        "SELECT start_quantity, quantity FROM stock_return_spans WHERE return_line_id = ?1 ORDER BY start_quantity",
    )?;
    stock_notes::read_quantity_share(&mut stmt, line, capacity, quantity)
}

pub(crate) fn require_available(
    conn: &Connection,
    company: i64,
    date: NaiveDate,
    line: &StockNoteLineDraft,
) -> Result<()> {
    let available = stock_bucket_quantity(
        conn,
        company,
        line.item_id,
        line.batch_id,
        Some(line.location_id),
        Some(date),
    )?;
    if i128::from(line.quantity) > available {
        return Err(Error::Validation(
            "Insufficient stock in the original location and batch on this date".into(),
        ));
    }
    Ok(())
}

impl Store {
    pub fn stock_note_returns(&self, company: i64, note: i64) -> Result<Vec<StockReturn>> {
        let mut headers = self.conn.prepare(
            "SELECT id, reference, date, reason, reversed_on, reversal_reason FROM stock_returns
             WHERE company_id = ?1 AND note_id = ?2 ORDER BY date DESC, id DESC",
        )?;
        let mut details = self.conn.prepare(
            "SELECT i.name, i.unit, loc.name, b.name, l.quantity
             FROM stock_return_lines l JOIN stock_note_lines src ON src.id = l.note_line_id
             JOIN stock_items i ON i.id = src.item_id JOIN stock_locations loc ON loc.id = src.location_id
             LEFT JOIN stock_batches b ON b.id = src.batch_id WHERE l.return_id = ?1 ORDER BY l.id",
        )?;
        let mut rows = headers.query(params![company, note])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            let lines = details
                .query_map([id], |row| {
                    Ok(StockReturnLine {
                        item: row.get(0)?,
                        unit: row.get(1)?,
                        location: row.get(2)?,
                        batch: row.get(3)?,
                        quantity: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            result.push(StockReturn {
                id,
                reference: row.get(1)?,
                date: row.get(2)?,
                reason: row.get(3)?,
                reversed_on: row.get(4)?,
                reversal_reason: row.get(5)?,
                lines,
            });
        }
        Ok(result)
    }

    pub fn post_stock_return(
        &mut self,
        company: i64,
        note: i64,
        date: &str,
        reference: &str,
        reason: &str,
        lines: &[StockReturnLineDraft],
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Return reference")?;
        let reason = required(reason, "Return reason")?;
        if lines.is_empty() {
            return Err(Error::Validation("Enter a quantity to return".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = stock_notes::get_note(&tx, company, note)?;
        if source.reversed_on.is_some() || date < parse_date(&source.date)? {
            return Err(Error::Validation(
                "Select an active stock note and a return date on or after its date".into(),
            ));
        }
        stock_notes::check_after_invoice_reversals(&tx, company, note, date, true)?;
        check_after_returns(&tx, company, note, date, true)?;
        let mut available = stock_notes::note_lines(&tx, company, note)?
            .into_iter()
            .map(|line| (line.id, line))
            .collect::<std::collections::HashMap<_, _>>();
        if tx.execute(
            "INSERT INTO stock_returns(company_id, note_id, date, reference, reference_key, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(note_id, reference_key) DO NOTHING",
            params![
                company,
                note,
                date.to_string(),
                reference,
                reference.to_lowercase(),
                reason
            ],
        )? != 1
        {
            return Err(Error::Validation(
                "A return with this reference already exists for the note".into(),
            ));
        }
        let return_id = tx.last_insert_rowid();
        let movement_reason = format!("Return {reference} from {}: {reason}", source.reference);
        for requested in lines {
            let mut line = available.remove(&requested.note_line).ok_or_else(|| {
                Error::Validation("Select each stock note line at most once".into())
            })?;
            if requested.quantity <= 0 || requested.quantity > line.unbilled() {
                return Err(Error::Validation(format!(
                    "Return quantity exceeds the unbilled stock for {}",
                    line.item
                )));
            }
            check_quantity_precision(&tx, company, line.values.item_id, requested.quantity)?;
            let spans = stock_notes::allocate_spans(&tx, &line, requested.quantity)?;
            line.values.quantity = requested.quantity;
            if source.kind == InvoiceKind::Purchase {
                require_available(&tx, company, date, &line.values)?;
            }
            let movement = stock_notes::insert_movement(
                &tx,
                company,
                date,
                &line.values,
                -stock_notes::stock_direction(source.kind),
                &movement_reason,
            )?;
            tx.execute(
                "INSERT INTO stock_return_lines(return_id, note_line_id, quantity, adjustment_id) VALUES (?1, ?2, ?3, ?4)",
                params![return_id, line.id, requested.quantity, movement],
            )?;
            let line_id = tx.last_insert_rowid();
            for span in spans {
                tx.execute(
                    "INSERT INTO stock_return_spans VALUES (?1, ?2, ?3)",
                    params![line_id, span.start, span.end - span.start],
                )?;
            }
        }
        tx.commit()?;
        Ok(return_id)
    }

    pub fn reverse_stock_return(
        &mut self,
        company: i64,
        id: i64,
        date: &str,
        reason: &str,
    ) -> Result<()> {
        let date = parse_date(date)?;
        let reason = required(reason, "Return reversal reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (note, reference, returned_on): (i64, String, String) = tx.query_row(
            "SELECT note_id, reference, date FROM stock_returns WHERE company_id = ?1 AND id = ?2 AND reversed_on IS NULL",
            params![company, id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Select an active stock return".into()))?;
        if date < parse_date(&returned_on)? {
            return Err(Error::Validation(
                "Reversal date cannot precede the return date".into(),
            ));
        }
        let source = stock_notes::get_note(&tx, company, note)?;
        let lines = {
            let mut stmt = tx.prepare(
                "SELECT l.id, src.item_id, src.location_id, src.batch_id, l.quantity, src.unit_price
                 FROM stock_return_lines l JOIN stock_note_lines src ON src.id = l.note_line_id
                 WHERE l.return_id = ?1 ORDER BY l.id",
            )?;
            let result = stmt
                .query_map([id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        StockNoteLineDraft {
                            item_id: row.get(1)?,
                            location_id: row.get(2)?,
                            batch_id: row.get(3)?,
                            quantity: row.get(4)?,
                            unit_price: row.get(5)?,
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            result
        };
        if let Some(order) = source.order_id {
            crate::orders::check_fulfilment(
                &tx,
                company,
                order,
                source.kind,
                source.party_ledger_id,
                date,
                crate::orders::FulfilmentChange::ReverseReturn,
                lines.iter().map(|(_, line)| (line.item_id, line.quantity)),
            )?;
        }
        let movement_reason = format!("Return {reference} reversal: {reason}");
        for (line_id, line) in lines {
            if source.kind == InvoiceKind::Sale {
                require_available(&tx, company, date, &line)?;
            }
            let movement = stock_notes::insert_movement(
                &tx,
                company,
                date,
                &line,
                stock_notes::stock_direction(source.kind),
                &movement_reason,
            )?;
            tx.execute(
                "UPDATE stock_return_lines SET reversal_adjustment_id = ?1 WHERE id = ?2",
                params![movement, line_id],
            )?;
        }
        tx.execute(
            "UPDATE stock_returns SET reversed_on = ?1, reversal_reason = ?2 WHERE id = ?3",
            params![date.to_string(), reason, id],
        )?;
        if let Some(order) = source.order_id {
            crate::stock_reservations::check_order_coverage(&tx, order)?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::StockReturnLineDraft;
    use crate::actual_cost::ActualCostMethod;
    use crate::inventory::{InvoiceDiscount, InvoiceDraft, InvoiceKind, InvoiceLine};
    use crate::orders::{OrderDraft, OrderLine};
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::{Category, Store};

    #[test]
    fn customer_return_reopens_order_quantity_and_restores_issue_cost() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Demo").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let sales = store
            .create_ledger(company, "Trade sales", Category::Income)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let line = StockNoteLineDraft {
            item_id: item,
            location_id: location,
            batch_id: None,
            quantity: 5000,
            unit_price: 2500,
        };
        let receipt = StockNoteDraft {
            kind: InvoiceKind::Purchase,
            date: "2026-01-01".into(),
            reference: "REC-01".into(),
            party_ledger_id: supplier,
            order_id: None,
            narration: String::new(),
            lines: vec![line.clone()],
        };
        store.post_stock_note(company, &receipt).unwrap();
        let order = store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Sale,
                    party_ledger_id: customer,
                    reference: "SO-01".into(),
                    date: "2026-01-01".into(),
                    expected_on: None,
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: item,
                        quantity: 5000,
                        unit_price: 3500,
                    }],
                },
            )
            .unwrap();
        let mut delivery = StockNoteDraft {
            kind: InvoiceKind::Sale,
            date: "2026-01-02".into(),
            reference: "DEL-01".into(),
            party_ledger_id: customer,
            order_id: Some(order),
            narration: String::new(),
            lines: vec![line],
        };
        let note = store.post_stock_note(company, &delivery).unwrap();
        let note_line = store.stock_note_lines(company, note).unwrap()[0].id;
        let quantity = StockReturnLineDraft {
            note_line,
            quantity: 2000,
        };
        let returned = store
            .post_stock_return(
                company,
                note,
                "2026-01-02",
                "R-01",
                "Declined items",
                &[quantity.clone()],
            )
            .unwrap();
        assert!(store
            .post_stock_return(
                company,
                note,
                "2026-01-02",
                "r-01",
                "Duplicate",
                &[quantity.clone()]
            )
            .is_err());
        assert!(store
            .reverse_stock_note(company, note, "2026-01-02", "Still returned")
            .is_err());
        assert_eq!(
            store.order_detail(company, order).unwrap().lines[0].fulfilled,
            3000
        );
        assert_eq!(
            store.stock_note_lines(company, note).unwrap()[0].unbilled(),
            3000
        );

        let mut invoice = InvoiceDraft {
            kind: InvoiceKind::Sale,
            date: "2026-01-02".into(),
            due_date: None,
            credit_days: None,
            reference: "INV-01".into(),
            bill_reference: None,
            narration: String::new(),
            party_ledger_id: customer,
            trade_ledger_id: sales,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: Vec::new(),
            tax_calculation: None,
            adjustments: Vec::new(),
            lines: vec![InvoiceLine {
                item_id: item,
                location_id: location,
                batch_id: None,
                trade_ledger_id: None,
                extra_allocations: Vec::new(),
                quantity: 5000,
                unit_price: 3500,
                discount: InvoiceDiscount::None,
            }],
        };
        assert!(store.invoice_stock_note(company, note, &invoice).is_err());
        invoice.lines[0].quantity = 3000;
        store.invoice_stock_note(company, note, &invoice).unwrap();
        assert_eq!(
            store.order_detail(company, order).unwrap().lines[0].billed,
            3000
        );
        for method in ActualCostMethod::ALL {
            let snapshot = store
                .stock_valuation_at_actual_cost(company, "2026-01-02", method)
                .unwrap();
            assert_eq!(snapshot.unavailable, 0);
            assert_eq!(snapshot.known_value, 5000);
            let margin = store
                .sales_at_actual_cost(company, None, Some("2026-01-02"), method)
                .unwrap();
            assert_eq!(margin.cost_of_sales, 7500);
        }

        delivery.date = "2026-01-01".into();
        delivery.reference = "REPLACE".into();
        delivery.lines[0].quantity = 2000;
        assert!(store.post_stock_note(company, &delivery).is_err());
        let closure = store
            .preclose_order_line(company, order, item, "2026-01-02", 1000, "Reduce order")
            .unwrap();
        assert!(store
            .reverse_stock_return(company, returned, "2026-01-03", "Over pending quantity")
            .is_err());
        store
            .reopen_order_line(company, order, closure, "2026-01-04", "Restore quantity")
            .unwrap();
        assert!(store
            .reverse_stock_return(company, returned, "2026-01-03", "Before reopening")
            .is_err());
        store
            .reverse_stock_return(company, returned, "2026-01-04", "Reaccepted")
            .unwrap();
        assert_eq!(
            store.order_detail(company, order).unwrap().lines[0].fulfilled,
            5000
        );
        assert_eq!(
            store.stock_note_lines(company, note).unwrap()[0].unbilled(),
            2000
        );
        invoice.reference = "INV-02".into();
        invoice.lines[0].quantity = 2000;
        assert!(store.invoice_stock_note(company, note, &invoice).is_err());
        invoice.date = "2026-01-04".into();
        store.invoice_stock_note(company, note, &invoice).unwrap();
        let snapshot = store
            .stock_valuation_at_actual_cost(company, "2026-01-04", ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(snapshot.unavailable, 0);
        assert_eq!(snapshot.known_value, 0);
    }

    #[test]
    fn supplier_return_and_reversal_are_dated_and_respect_period_locks() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Demo").unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let note = store
            .post_stock_note(
                company,
                &StockNoteDraft {
                    kind: InvoiceKind::Purchase,
                    date: "2026-01-01".into(),
                    reference: "REC-01".into(),
                    party_ledger_id: supplier,
                    order_id: None,
                    narration: String::new(),
                    lines: vec![StockNoteLineDraft {
                        item_id: item,
                        location_id: location,
                        batch_id: None,
                        quantity: 3000,
                        unit_price: 3000,
                    }],
                },
            )
            .unwrap();
        let note_line = store.stock_note_lines(company, note).unwrap()[0].id;
        let draft = [StockReturnLineDraft {
            note_line,
            quantity: 1000,
        }];
        store
            .set_period_lock(company, Some("2026-01-01"), "Close opening day")
            .unwrap();
        assert!(store
            .post_stock_return(company, note, "2026-01-01", "R-01", "Reject", &draft)
            .is_err());
        let returned = store
            .post_stock_return(company, note, "2026-01-02", "R-01", "Reject", &draft)
            .unwrap();
        store
            .reverse_stock_return(company, returned, "2026-01-03", "Kept original goods")
            .unwrap();
        for (date, returned, unbilled, value) in [
            ("2026-01-01", 0, 3000, 9000),
            ("2026-01-02", 1000, 2000, 6000),
            ("2026-01-03", 0, 3000, 9000),
        ] {
            let report = store.receipt_reconciliation(company, Some(date)).unwrap();
            assert_eq!(report[0].returned_quantity, returned);
            assert_eq!(report[0].unbilled_quantity(), unbilled);
            for method in ActualCostMethod::ALL {
                let snapshot = store
                    .stock_valuation_at_actual_cost(company, date, method)
                    .unwrap();
                assert_eq!(snapshot.unavailable, 0);
                assert_eq!(snapshot.known_value, value);
            }
        }
        assert_eq!(store.stock_note_returns(company, note).unwrap().len(), 1);
        assert_eq!(store.stock_receipts(company, None).unwrap().len(), 1);
        assert!(store.vouchers(company).unwrap().is_empty());
    }
}
