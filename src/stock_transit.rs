//! Dated dispatches, partial receipts and returns for internal stock transfers.
use crate::inventory::{
    check_quantity_precision, check_stock_after_decrease, check_stock_batch, check_stock_location,
    stock_bucket_quantity,
};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE stock_transfer_dispatches(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            batch_id INTEGER REFERENCES stock_batches(id),
            from_location_id INTEGER NOT NULL REFERENCES stock_locations(id),
            to_location_id INTEGER NOT NULL REFERENCES stock_locations(id),
            date TEXT NOT NULL,
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            reason TEXT NOT NULL,
            CHECK(from_location_id != to_location_id),
            UNIQUE(company_id, reference_key)
        );
        CREATE INDEX stock_transfer_dispatches_stock
            ON stock_transfer_dispatches(company_id, item_id, from_location_id, batch_id);
        CREATE TABLE stock_transfer_arrivals(
            id INTEGER PRIMARY KEY,
            dispatch_id INTEGER NOT NULL REFERENCES stock_transfer_dispatches(id),
            company_id INTEGER NOT NULL REFERENCES companies(id),
            date TEXT NOT NULL,
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            returned INTEGER NOT NULL CHECK(returned IN (0, 1)),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            reason TEXT NOT NULL,
            UNIQUE(company_id, reference_key)
        );
        CREATE INDEX stock_transfer_arrivals_dispatch ON stock_transfer_arrivals(dispatch_id);
        CREATE VIEW stock_transfer_location_movements AS
            SELECT id, company_id, item_id, batch_id, date, -quantity AS quantity,
                   from_location_id AS location_id, from_location_id, to_location_id,
                   'Transfer' AS kind, NULL AS reference, reason
            FROM stock_transfers
            UNION ALL
            SELECT id, company_id, item_id, batch_id, date, quantity,
                   to_location_id, from_location_id, to_location_id, 'Transfer', NULL, reason
            FROM stock_transfers
            UNION ALL
            SELECT id, company_id, item_id, batch_id, date, -quantity,
                   from_location_id, from_location_id, to_location_id, 'Dispatch', reference, reason
            FROM stock_transfer_dispatches
            UNION ALL
            SELECT a.id, a.company_id, d.item_id, d.batch_id, a.date, a.quantity,
                   CASE a.returned WHEN 1 THEN d.from_location_id ELSE d.to_location_id END,
                   d.from_location_id, d.to_location_id,
                   CASE a.returned WHEN 1 THEN 'Return to source' ELSE 'Receipt' END,
                   a.reference, a.reason
            FROM stock_transfer_arrivals a
            JOIN stock_transfer_dispatches d ON d.id = a.dispatch_id AND d.company_id = a.company_id;",
    )?;
    for table in ["stock_transfer_dispatches", "stock_transfer_arrivals"] {
        crate::periods::protect_dated_table(conn, table)?;
    }
    Ok(())
}

pub struct StockDispatchDraft {
    pub item_id: i64,
    pub batch_id: Option<i64>,
    pub from_location_id: i64,
    pub to_location_id: i64,
    pub date: String,
    pub quantity: i64,
    pub reference: String,
    pub reason: String,
}

pub struct StockArrivalDraft {
    pub date: String,
    pub quantity: i64,
    pub returned: bool,
    pub reference: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct StockDispatch {
    pub id: i64,
    pub date: String,
    pub reference: String,
    pub item: String,
    pub unit: String,
    pub batch: Option<String>,
    pub from: String,
    pub to: String,
    pub quantity: i64,
    pub received: i64,
    pub returned: i64,
    pub reason: String,
}

impl StockDispatch {
    pub fn in_transit(&self) -> i64 {
        self.quantity - self.received - self.returned
    }
}

#[derive(Debug, Clone)]
pub struct StockArrival {
    pub date: String,
    pub reference: String,
    pub quantity: i64,
    pub returned: bool,
    pub reason: String,
}

pub(crate) fn in_transit_by_item(conn: &Connection, company: i64) -> Result<HashMap<i64, i128>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.item_id, d.quantity - COALESCE((
            SELECT SUM(a.quantity) FROM stock_transfer_arrivals a WHERE a.dispatch_id = d.id
         ), 0) FROM stock_transfer_dispatches d WHERE d.company_id = ?1",
    )?;
    let mut totals = HashMap::<_, i128>::new();
    for row in stmt.query_map([company], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (item, pending) = row?;
        *totals.entry(item).or_default() += i128::from(pending);
    }
    Ok(totals)
}

fn arrival_totals(
    conn: &Connection,
    company: i64,
    cutoff: Option<NaiveDate>,
) -> Result<HashMap<i64, (i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT dispatch_id, date, quantity, returned FROM stock_transfer_arrivals WHERE company_id = ?1",
    )?;
    let mut totals = HashMap::<_, (i64, i64)>::new();
    for row in stmt.query_map([company], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, bool>(3)?,
        ))
    })? {
        let (id, date, quantity, returned) = row?;
        let date = parse_date(&date)?;
        if cutoff.is_none_or(|end| date <= end) {
            let (receipts, returns) = totals.entry(id).or_default();
            if returned {
                *returns += quantity;
            } else {
                *receipts += quantity;
            }
        }
    }
    Ok(totals)
}

impl Store {
    pub fn dispatch_stock(&mut self, company: i64, draft: &StockDispatchDraft) -> Result<i64> {
        let date = parse_date(&draft.date)?;
        let reference = required(&draft.reference, "Dispatch reference")?;
        let reason = required(&draft.reason, "Dispatch reason")?;
        if draft.quantity <= 0 || draft.from_location_id == draft.to_location_id {
            return Err(Error::Validation(
                "Choose different locations and a positive quantity".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::advances::check_open_date(&tx, company, date)?;
        check_quantity_precision(&tx, company, draft.item_id, draft.quantity)?;
        check_stock_batch(&tx, company, draft.item_id, draft.batch_id)?;
        check_stock_location(&tx, company, draft.from_location_id)?;
        check_stock_location(&tx, company, draft.to_location_id)?;
        let available = stock_bucket_quantity(
            &tx,
            company,
            draft.item_id,
            draft.batch_id,
            Some(draft.from_location_id),
            Some(date),
        )?;
        if available < i128::from(draft.quantity) {
            return Err(Error::Validation(
                "The source location and batch have insufficient stock on the dispatch date".into(),
            ));
        }
        let inserted = tx.execute(
            "INSERT INTO stock_transfer_dispatches(company_id, item_id, batch_id, from_location_id,
                 to_location_id, date, quantity, reference, reference_key, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(company_id, reference_key) DO NOTHING",
            params![
                company,
                draft.item_id,
                draft.batch_id,
                draft.from_location_id,
                draft.to_location_id,
                date.to_string(),
                draft.quantity,
                reference,
                reference.to_lowercase(),
                reason
            ],
        )?;
        if inserted != 1 {
            return Err(Error::Validation(
                "A dispatch already uses this reference".into(),
            ));
        }
        let dispatch = tx.last_insert_rowid();
        check_stock_after_decrease(
            &tx,
            company,
            draft.item_id,
            draft.from_location_id,
            draft.batch_id,
        )?;
        tx.commit()?;
        Ok(dispatch)
    }

    pub fn record_stock_arrival(
        &mut self,
        company: i64,
        dispatch: i64,
        draft: &StockArrivalDraft,
    ) -> Result<i64> {
        let date = parse_date(&draft.date)?;
        let reference = required(&draft.reference, "Arrival reference")?;
        let reason = required(&draft.reason, "Arrival reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::advances::check_open_date(&tx, company, date)?;
        let (item, dispatched_on, pending): (i64, String, i64) = tx
            .query_row(
                "SELECT d.item_id, d.date, d.quantity - COALESCE((
                SELECT SUM(a.quantity) FROM stock_transfer_arrivals a WHERE a.dispatch_id = d.id
             ), 0) FROM stock_transfer_dispatches d WHERE d.company_id = ?1 AND d.id = ?2",
                params![company, dispatch],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Dispatch not found in this company".into()))?;
        if date < parse_date(&dispatched_on)? {
            return Err(Error::Validation(
                "Arrival date cannot precede the dispatch".into(),
            ));
        }
        if draft.quantity <= 0 || draft.quantity > pending {
            return Err(Error::Validation(
                "Enter a positive quantity within the amount still in transit".into(),
            ));
        }
        check_quantity_precision(&tx, company, item, draft.quantity)?;
        let inserted = tx.execute(
            "INSERT INTO stock_transfer_arrivals(dispatch_id, company_id, date, quantity, returned,
                 reference, reference_key, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(company_id, reference_key) DO NOTHING",
            params![
                dispatch,
                company,
                date.to_string(),
                draft.quantity,
                draft.returned,
                reference,
                reference.to_lowercase(),
                reason
            ],
        )?;
        if inserted != 1 {
            return Err(Error::Validation(
                "An arrival already uses this reference".into(),
            ));
        }
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Transfer balances and history through an inclusive date, or across all saved dates.
    pub fn stock_dispatches(
        &self,
        company: i64,
        as_of: Option<&str>,
    ) -> Result<Vec<StockDispatch>> {
        let cutoff = as_of.map(parse_date).transpose()?;
        let tx = self.conn.unchecked_transaction()?;
        let arrivals = arrival_totals(&tx, company, cutoff)?;
        let mut dispatches = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT d.id, d.date, d.reference, s.name, s.unit, b.name, f.name, t.name, d.quantity, d.reason
                 FROM stock_transfer_dispatches d JOIN stock_items s ON s.id = d.item_id
                 JOIN stock_locations f ON f.id = d.from_location_id JOIN stock_locations t ON t.id = d.to_location_id
                 LEFT JOIN stock_batches b ON b.id = d.batch_id WHERE d.company_id = ?1",
            )?;
            for row in stmt.query_map([company], |r| {
                let id = r.get(0)?;
                let (received, returned) = arrivals.get(&id).copied().unwrap_or_default();
                Ok(StockDispatch {
                    id,
                    date: r.get(1)?,
                    reference: r.get(2)?,
                    item: r.get(3)?,
                    unit: r.get(4)?,
                    batch: r.get(5)?,
                    from: r.get(6)?,
                    to: r.get(7)?,
                    quantity: r.get(8)?,
                    reason: r.get(9)?,
                    received,
                    returned,
                })
            })? {
                let dispatch = row?;
                let date = parse_date(&dispatch.date)?;
                if cutoff.is_none_or(|end| date <= end) {
                    dispatches.push((date, dispatch));
                }
            }
        }
        tx.commit()?;
        dispatches.sort_by_key(|(date, dispatch)| std::cmp::Reverse((*date, dispatch.id)));
        Ok(dispatches
            .into_iter()
            .map(|(_, dispatch)| dispatch)
            .collect())
    }

    pub fn stock_arrivals(
        &self,
        company: i64,
        dispatch: i64,
        as_of: Option<&str>,
    ) -> Result<Vec<StockArrival>> {
        let cutoff = as_of.map(parse_date).transpose()?;
        let mut stmt = self.conn.prepare(
            "SELECT date, reference, quantity, returned, reason FROM stock_transfer_arrivals
             WHERE company_id = ?1 AND dispatch_id = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![company, dispatch], |r| {
            Ok(StockArrival {
                date: r.get(0)?,
                reference: r.get(1)?,
                quantity: r.get(2)?,
                returned: r.get(3)?,
                reason: r.get(4)?,
            })
        })?;
        let mut arrivals = Vec::new();
        for row in rows {
            let arrival = row?;
            let date = parse_date(&arrival.date)?;
            if cutoff.is_none_or(|end| date <= end) {
                arrivals.push((date, arrival));
            }
        }
        arrivals.sort_by_key(|(date, _)| *date);
        Ok(arrivals.into_iter().map(|(_, arrival)| arrival).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actual_cost::ActualCostMethod;
    use crate::inventory::InvoiceKind;
    use crate::orders::{OrderDraft, OrderLine};
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::stock_reservations::StockReservationDraft;
    use crate::Category;

    #[test]
    fn transit_reconciles_dated_warehouse_movements_and_company_cost() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let other_company = store.create_company("Other").unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let from = store.stock_locations(company).unwrap()[0].id;
        let to = store
            .create_stock_location_under(company, "Annex", Some(from))
            .unwrap();
        let batch = store
            .create_stock_batch(company, item, "Lot 1", None, None)
            .unwrap();
        store
            .post_stock_note(
                company,
                &StockNoteDraft {
                    kind: InvoiceKind::Purchase,
                    date: "2026-01-01".into(),
                    reference: "OPENING".into(),
                    party_ledger_id: supplier,
                    order_id: None,
                    narration: String::new(),
                    lines: vec![StockNoteLineDraft {
                        item_id: item,
                        location_id: from,
                        batch_id: Some(batch),
                        quantity: 10_000,
                        unit_price: 2500,
                    }],
                },
            )
            .unwrap();
        store
            .transfer_stock_batch(
                company,
                item,
                Some(batch),
                from,
                to,
                "2026-01-02",
                1000,
                "On site",
            )
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE bill_split_advances;
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
             PRAGMA user_version = 67;",
            )
            .unwrap();
        let mut store = Store::from_connection(store.conn).unwrap();
        assert_eq!(
            store.stock_location_balances(company, to, None).unwrap()[0].quantity,
            1000
        );
        assert_eq!(store.stock_transfers(company).unwrap().len(), 1);
        assert!(store.stock_dispatches(company, None).unwrap().is_empty());
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
                        unit_price: 3000,
                    }],
                },
            )
            .unwrap();
        let hold = store
            .reserve_order_stock(
                company,
                order,
                &StockReservationDraft {
                    item_id: item,
                    location_id: from,
                    batch_id: Some(batch),
                    quantity: 4000,
                    reason: "Hold".into(),
                },
            )
            .unwrap();

        let mut draft = StockDispatchDraft {
            item_id: item,
            batch_id: Some(batch),
            from_location_id: from,
            to_location_id: to,
            date: "2026-01-03".into(),
            quantity: 6000,
            reference: "MOVE-01".into(),
            reason: "Warehouse transfer".into(),
        };
        assert!(
            store.dispatch_stock(company, &draft).is_err(),
            "source has reservations"
        );
        assert!(store.stock_dispatches(company, None).unwrap().is_empty());
        store
            .release_order_stock(company, order, hold, 2000, "Release excess")
            .unwrap();
        let dispatch = store.dispatch_stock(company, &draft).unwrap();
        draft.quantity = 1000;
        draft.reference = "move-01".into();
        assert!(
            store.dispatch_stock(company, &draft).is_err(),
            "duplicate reference"
        );
        assert!(store
            .stock_dispatches(company, Some("2026-01-02"))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .stock_batch_quantity(company, item, batch, Some(from), Some("2026-01-03"))
                .unwrap(),
            3000
        );
        assert_eq!(
            store
                .stock_batch_quantity(company, item, batch, Some(to), Some("2026-01-03"))
                .unwrap(),
            1000
        );
        assert_eq!(
            store
                .stock_batch_quantity(company, item, batch, None, Some("2026-01-03"))
                .unwrap(),
            10_000
        );
        let report = store.order_availability(company).unwrap().remove(0);
        assert_eq!(
            (
                report.on_hand,
                report.in_transit,
                report.reserved,
                report.unreserved()
            ),
            (10_000, 6000, 2000, 2000)
        );
        let value = store
            .stock_valuation_at_actual_cost(company, "2026-01-03", ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(
            (value.rows[0].quantity, value.known_value, value.unavailable),
            (10_000, 25_000, 0)
        );
        let branch = store
            .stock_register_in_scope(company, item, Some(from), true, None, Some("2026-01-03"))
            .unwrap();
        assert_eq!(branch.closing_quantity, 4000);
        assert_eq!(branch.movements.len(), 2);
        assert!(branch.movements[1].description.contains("Dispatch MOVE-01"));

        let mut receipt = StockArrivalDraft {
            date: "2026-01-02".into(),
            quantity: 2000,
            returned: false,
            reference: "ARR-01".into(),
            reason: "First load".into(),
        };
        assert!(store
            .record_stock_arrival(company, dispatch, &receipt)
            .is_err());
        store
            .set_period_lock(company, Some("2026-01-03"), "Close period")
            .unwrap();
        receipt.date = "2026-01-03".into();
        assert!(store
            .record_stock_arrival(company, dispatch, &receipt)
            .is_err());
        receipt.date = "2026-01-04".into();
        assert!(store
            .record_stock_arrival(other_company, dispatch, &receipt)
            .is_err());
        store
            .record_stock_arrival(company, dispatch, &receipt)
            .unwrap();
        assert!(store
            .record_stock_arrival(company, dispatch, &receipt)
            .is_err());
        receipt.date = "2026-01-05".into();
        receipt.reference = "RET-01".into();
        receipt.quantity = 1000;
        receipt.returned = true;
        store
            .record_stock_arrival(company, dispatch, &receipt)
            .unwrap();
        let rows = store.stock_dispatches(company, Some("2026-01-04")).unwrap();
        assert_eq!(
            (rows[0].received, rows[0].returned, rows[0].in_transit()),
            (2000, 0, 4000)
        );
        assert_eq!(
            store
                .stock_arrivals(company, dispatch, Some("2026-01-04"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store.stock_location_balances(company, from, None).unwrap()[0].quantity,
            4000
        );
        assert_eq!(
            store.stock_location_balances(company, to, None).unwrap()[0].quantity,
            3000
        );

        receipt.date = "2026-01-06".into();
        receipt.reference = "ARR-02".into();
        receipt.returned = false;
        receipt.quantity = 4000;
        assert!(store
            .record_stock_arrival(company, dispatch, &receipt)
            .is_err());
        receipt.quantity = 3000;
        store
            .record_stock_arrival(company, dispatch, &receipt)
            .unwrap();
        assert_eq!(
            store.stock_dispatches(company, None).unwrap()[0].in_transit(),
            0
        );
        assert_eq!(
            store.stock_location_balances(company, to, None).unwrap()[0].quantity,
            6000
        );
        assert_eq!(
            store.stock_balances(company, None).unwrap()[0].quantity,
            10_000
        );
        assert_eq!(store.order_availability(company).unwrap()[0].in_transit, 0);
    }
}
