//! Location and batch commitments for pending sales order quantities.
use crate::inventory::{
    check_quantity_precision, check_stock_batch, check_stock_location, stock_bucket_changes,
};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashMap};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE stock_reservations(
            id INTEGER PRIMARY KEY,
            order_line_id INTEGER NOT NULL REFERENCES order_lines(id),
            location_id INTEGER NOT NULL REFERENCES stock_locations(id),
            batch_id INTEGER REFERENCES stock_batches(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            reserved_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            reason TEXT NOT NULL
        );
        CREATE INDEX stock_reservations_line ON stock_reservations(order_line_id);
        CREATE INDEX stock_reservations_bucket ON stock_reservations(location_id, batch_id);
        CREATE TABLE stock_reservation_releases(
            id INTEGER PRIMARY KEY,
            reservation_id INTEGER NOT NULL REFERENCES stock_reservations(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            released_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            fulfilled INTEGER NOT NULL CHECK(fulfilled IN (0, 1)),
            reason TEXT NOT NULL
        );
        CREATE INDEX stock_reservation_releases_reservation ON stock_reservation_releases(reservation_id);
        CREATE VIEW stock_reservation_balances AS
            SELECT r.*, o.id AS order_id, o.company_id, ol.item_id,
                CASE WHEN o.closed_at IS NULL THEN r.quantity - COALESCE((
                    SELECT SUM(e.quantity) FROM stock_reservation_releases e WHERE e.reservation_id = r.id
                ), 0) ELSE 0 END AS remaining
            FROM stock_reservations r JOIN order_lines ol ON ol.id = r.order_line_id
            JOIN orders o ON o.id = ol.order_id;",
    )?;
    Ok(())
}

pub(crate) fn migrate_expiry(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE stock_reservation_expiries(
            reservation_id INTEGER PRIMARY KEY REFERENCES stock_reservations(id),
            expires_on TEXT NOT NULL
        );
        CREATE VIEW active_stock_reservation_balances AS
            SELECT r.id, r.order_line_id, r.order_id, r.company_id, r.item_id, r.location_id,
                   r.batch_id, r.quantity, r.reserved_at, r.reason, e.expires_on,
                   CASE WHEN e.expires_on < date('now', 'localtime') THEN 0 ELSE r.remaining END AS remaining
            FROM stock_reservation_balances r
            LEFT JOIN stock_reservation_expiries e ON e.reservation_id = r.id;",
    )?;
    Ok(())
}

pub(crate) fn record_expiry(
    conn: &Connection,
    reservation: i64,
    expires_on: Option<&str>,
) -> Result<()> {
    if let Some(expires_on) = expires_on {
        let date = valid_expiry(expires_on, local_date(conn)?)?;
        conn.execute(
            "INSERT INTO stock_reservation_expiries(reservation_id, expires_on) VALUES (?1, ?2)",
            params![reservation, date.to_string()],
        )?;
    }
    Ok(())
}

fn local_date(conn: &Connection) -> Result<NaiveDate> {
    let today: String = conn.query_row("SELECT date('now', 'localtime')", [], |r| r.get(0))?;
    parse_date(&today)
}

fn valid_expiry(expires_on: &str, today: NaiveDate) -> Result<NaiveDate> {
    let date = parse_date(expires_on)?;
    if date < today || chrono::Datelike::year(&date) > 9999 {
        return Err(Error::Validation(
            "Expiry must be today or later, with a four-digit year".into(),
        ));
    }
    Ok(date)
}

pub struct StockReservationDraft {
    pub item_id: i64,
    pub location_id: i64,
    pub batch_id: Option<i64>,
    pub quantity: i64,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct StockReservation {
    pub id: i64,
    pub item_id: i64,
    pub item: String,
    pub unit: String,
    pub location_id: i64,
    pub location: String,
    pub batch_id: Option<i64>,
    pub batch: Option<String>,
    pub quantity: i64,
    pub remaining: i64,
    pub reserved_at: String,
    pub reason: String,
    pub expires_on: Option<String>,
    pub expired_quantity: i64,
}

#[derive(Debug, Clone)]
pub struct StockReservationRelease {
    pub quantity: i64,
    pub released_at: String,
    pub fulfilled: bool,
    pub reason: String,
}

pub(crate) fn order_reservations(
    conn: &Connection,
    company: i64,
    order: i64,
) -> Result<Vec<StockReservation>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.id, r.item_id, s.name, s.unit, r.location_id, l.name,
                r.batch_id, b.name, r.quantity, r.remaining, r.reserved_at, r.reason, r.expires_on,
                CASE WHEN r.expires_on < date('now', 'localtime')
                     AND (o.closed_at IS NULL OR date(o.closed_at, 'localtime') > r.expires_on)
                     THEN r.quantity - COALESCE((
                         SELECT SUM(e.quantity) FROM stock_reservation_releases e WHERE e.reservation_id = r.id
                     ), 0) ELSE 0 END
         FROM active_stock_reservation_balances r JOIN stock_items s ON s.id = r.item_id
         JOIN orders o ON o.id = r.order_id
         JOIN stock_locations l ON l.id = r.location_id LEFT JOIN stock_batches b ON b.id = r.batch_id
         WHERE r.company_id = ?1 AND r.order_id = ?2 ORDER BY r.id",
    )?;
    let rows = stmt.query_map(params![company, order], |row| {
        Ok(StockReservation {
            id: row.get(0)?,
            item_id: row.get(1)?,
            item: row.get(2)?,
            unit: row.get(3)?,
            location_id: row.get(4)?,
            location: row.get(5)?,
            batch_id: row.get(6)?,
            batch: row.get(7)?,
            quantity: row.get(8)?,
            remaining: row.get(9)?,
            reserved_at: row.get(10)?,
            reason: row.get(11)?,
            expires_on: row.get(12)?,
            expired_quantity: row.get(13)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn reserved_by_item(conn: &Connection, company: i64) -> Result<HashMap<i64, i128>> {
    let mut stmt = conn.prepare_cached(
        "SELECT item_id, remaining FROM active_stock_reservation_balances WHERE company_id = ?1",
    )?;
    let mut quantities = HashMap::<_, i128>::new();
    let rows = stmt.query_map([company], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (item, quantity) = row?;
        *quantities.entry(item).or_default() += i128::from(quantity);
    }
    Ok(quantities)
}

fn reservation_margin(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
    existing_only: bool,
    expires_on: Option<&str>,
) -> Result<i128> {
    let today = local_date(conn)?;
    let through = expires_on
        .map(|date| valid_expiry(date, today))
        .transpose()?;
    let mut stmt = conn.prepare_cached(
        "SELECT remaining, expires_on FROM active_stock_reservation_balances
         WHERE company_id = ?1 AND item_id = ?2 AND location_id = ?3 AND batch_id IS ?4
           AND remaining > 0",
    )?;
    let mut reserved = 0_i128;
    let mut expiries = BTreeMap::<NaiveDate, i128>::new();
    for row in stmt.query_map(params![company, item, location, batch], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
    })? {
        let (quantity, expiry) = row?;
        reserved += i128::from(quantity);
        if let Some(expiry) = expiry {
            *expiries.entry(parse_date(&expiry)?).or_default() += i128::from(quantity);
        }
    }
    if existing_only && reserved == 0 {
        return Ok(0);
    }
    let mut expiries = expiries.into_iter().peekable();
    let mut margin = i128::MAX;
    for (date, stock) in
        stock_bucket_changes(conn, company, item, location, batch, today)?.closing_balances(today)
    {
        if through.is_some_and(|expiry| date > expiry) {
            break;
        }
        while expiries.peek().is_some_and(|(expiry, _)| *expiry < date) {
            let (_, released) = expiries.next().expect("peeked expiry");
            reserved -= released;
        }
        if !existing_only || reserved > 0 {
            margin = margin.min(stock - reserved);
        }
    }
    Ok(margin)
}

pub(crate) fn available_to_reserve(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
    expires_on: Option<&str>,
) -> Result<i128> {
    Ok(reservation_margin(conn, company, item, location, batch, false, expires_on)?.max(0))
}

pub(crate) fn check_stock_coverage(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
) -> Result<()> {
    if reservation_margin(conn, company, item, location, batch, true, None)? < 0 {
        return Err(Error::Validation(
            "Insufficient unreserved stock in this location and batch. Check the sales order reservations."
                .into(),
        ));
    }
    Ok(())
}

/// Check after a change to pending quantities. It must leave room for every remaining hold.
pub(crate) fn check_order_coverage(conn: &Connection, order: i64) -> Result<()> {
    let over_reserved: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM order_line_availability p
            JOIN active_stock_reservation_balances r ON r.order_line_id = p.id AND r.remaining > 0
            WHERE p.order_id = ?1
            GROUP BY p.id
            HAVING SUM(r.remaining) > p.quantity - p.fulfilled - p.preclosed
        )",
        [order],
        |r| r.get(0),
    )?;
    if over_reserved {
        return Err(Error::Validation(
            "Release the excess reservation or use its location and batch before reducing the pending quantity."
                .into(),
        ));
    }
    Ok(())
}

fn record_release(
    conn: &Connection,
    reservation: i64,
    quantity: i64,
    fulfilled: bool,
    reason: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO stock_reservation_releases(reservation_id, quantity, fulfilled, reason)
         VALUES (?1, ?2, ?3, ?4)",
        params![reservation, quantity, fulfilled, reason],
    )?;
    Ok(())
}

/// Consume this order's oldest active holds in the location and batch being shipped.
pub(crate) fn consume_reservations(
    conn: &Connection,
    order: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
    mut quantity: i64,
    reference: &str,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, remaining FROM active_stock_reservation_balances
         WHERE order_id = ?1 AND item_id = ?2 AND location_id = ?3
           AND batch_id IS ?4 AND remaining > 0 ORDER BY id",
    )?;
    let reservations = stmt
        .query_map(params![order, item, location, batch], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, i64)>>>()?;
    for (id, remaining) in reservations {
        let used = quantity.min(remaining);
        record_release(conn, id, used, true, reference)?;
        quantity -= used;
        if quantity == 0 {
            break;
        }
    }
    Ok(())
}

pub(crate) fn insert_reservation(
    conn: &Connection,
    company: i64,
    order: i64,
    draft: &StockReservationDraft,
) -> Result<i64> {
    let reason = required(&draft.reason, "Reservation reason")?;
    if draft.quantity <= 0 {
        return Err(Error::Validation(
            "Reservation quantity must be positive".into(),
        ));
    }
    check_quantity_precision(conn, company, draft.item_id, draft.quantity)?;
    check_stock_location(conn, company, draft.location_id)?;
    check_stock_batch(conn, company, draft.item_id, draft.batch_id)?;
    let inserted = conn.execute(
            "INSERT INTO stock_reservations(order_line_id, location_id, batch_id, quantity, reason)
             SELECT p.id, ?4, ?5, ?6, ?7 FROM order_line_availability p
             JOIN orders o ON o.id = p.order_id
             WHERE o.company_id = ?1 AND o.id = ?2 AND p.item_id = ?3
               AND o.kind = 'Sale' AND o.closed_at IS NULL
               AND p.quantity - p.fulfilled - p.preclosed - COALESCE((
                   SELECT SUM(r.remaining) FROM active_stock_reservation_balances r WHERE r.order_line_id = p.id
               ), 0) >= ?6",
            params![company, order, draft.item_id, draft.location_id, draft.batch_id, draft.quantity, reason],
        )?;
    if inserted != 1 {
        return Err(Error::Validation(
            "Select a sales order line with enough unreserved pending quantity".into(),
        ));
    }
    Ok(conn.last_insert_rowid())
}

impl Store {
    pub fn reserve_order_stock(
        &mut self,
        company: i64,
        order: i64,
        draft: &StockReservationDraft,
    ) -> Result<i64> {
        self.reserve_order_stock_until(company, order, draft, None)
    }

    pub fn reserve_order_stock_until(
        &mut self,
        company: i64,
        order: i64,
        draft: &StockReservationDraft,
        expires_on: Option<&str>,
    ) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reservation = insert_reservation(&tx, company, order, draft)?;
        record_expiry(&tx, reservation, expires_on)?;
        check_stock_coverage(
            &tx,
            company,
            draft.item_id,
            draft.location_id,
            draft.batch_id,
        )?;
        tx.commit()?;
        Ok(reservation)
    }

    pub fn release_order_stock(
        &mut self,
        company: i64,
        order: i64,
        reservation: i64,
        quantity: i64,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Release reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (item, remaining): (i64, i64) = tx
            .query_row(
                "SELECT item_id, remaining FROM active_stock_reservation_balances
                 WHERE company_id = ?1 AND order_id = ?2 AND id = ?3",
                params![company, order, reservation],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Validation("Reservation not found on this order".into()))?;
        if quantity <= 0 || quantity > remaining {
            return Err(Error::Validation(
                "Enter a quantity within the active reservation".into(),
            ));
        }
        check_quantity_precision(&tx, company, item, quantity)?;
        record_release(&tx, reservation, quantity, false, reason)?;
        tx.commit()?;
        Ok(())
    }

    pub fn stock_reservation_history(
        &self,
        company: i64,
        order: i64,
        reservation: i64,
    ) -> Result<Vec<StockReservationRelease>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT e.quantity, e.released_at, e.fulfilled, e.reason
             FROM stock_reservation_releases e
             JOIN active_stock_reservation_balances r ON r.id = e.reservation_id
             WHERE r.company_id = ?1 AND r.order_id = ?2 AND r.id = ?3
             ORDER BY e.id",
        )?;
        let rows = stmt.query_map(params![company, order, reservation], |r| {
            Ok(StockReservationRelease {
                quantity: r.get(0)?,
                released_at: r.get(1)?,
                fulfilled: r.get(2)?,
                reason: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn expiry_releases_remaining_stock_and_keeps_the_hold() {
        let mut store = crate::Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let party = store
            .create_ledger(company, "Customer", crate::Category::Asset)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        store
            .adjust_stock(company, item, "2026-01-01", 5000, "Opening")
            .unwrap();
        let order = store
            .create_order(
                company,
                &crate::orders::OrderDraft {
                    kind: crate::inventory::InvoiceKind::Sale,
                    party_ledger_id: party,
                    reference: "SO-01".into(),
                    date: "2026-01-01".into(),
                    expected_on: None,
                    notes: String::new(),
                    lines: vec![crate::orders::OrderLine {
                        item_id: item,
                        quantity: 5000,
                        unit_price: 100,
                    }],
                },
            )
            .unwrap();
        let draft = super::StockReservationDraft {
            item_id: item,
            location_id: location,
            batch_id: None,
            quantity: 4000,
            reason: "Hold".into(),
        };
        assert!(store
            .reserve_order_stock_until(company, order, &draft, Some("2000-01-01"))
            .is_err());
        let today: String = store
            .conn
            .query_row("SELECT date('now','localtime')", [], |r| r.get(0))
            .unwrap();
        let id = store
            .reserve_order_stock_until(company, order, &draft, Some(&today))
            .unwrap();
        assert_eq!(store.order_availability(company).unwrap()[0].reserved, 4000);
        assert!(store
            .adjust_stock(company, item, &today, -2000, "Issue while held")
            .is_err());
        let tomorrow = super::parse_date(&today)
            .unwrap()
            .succ_opt()
            .unwrap()
            .to_string();
        store
            .adjust_stock(company, item, &tomorrow, -3000, "Issue after expiry")
            .unwrap();
        store
            .release_order_stock(company, order, id, 1000, "Partial release")
            .unwrap();
        store.conn.execute(
            "UPDATE stock_reservation_expiries SET expires_on = date('now','localtime','-1 day') WHERE reservation_id = ?1",
            [id],
        ).unwrap();
        assert_eq!(store.order_availability(company).unwrap()[0].reserved, 0);
        let detail = store.order_detail(company, order).unwrap();
        assert_eq!(detail.reservations[0].id, id);
        assert_eq!(detail.reservations[0].remaining, 0);
        assert_eq!(detail.reservations[0].expired_quantity, 3000);
        assert_eq!(
            store.stock_reservation_history(company, order, id).unwrap()[0].quantity,
            1000
        );
        assert_eq!(detail.lines[0].unfulfilled(), 5000);
        store
            .reserve_order_stock(
                company,
                order,
                &super::StockReservationDraft {
                    quantity: 2000,
                    ..draft
                },
            )
            .unwrap();
        store
            .close_order(company, order, "Order cancelled")
            .unwrap();
        assert_eq!(
            store.order_detail(company, order).unwrap().reservations[0].expired_quantity,
            3000
        );
    }
    use super::*;
    use crate::inventory::{
        InvoiceAllocation, InvoiceDiscount, InvoiceDraft, InvoiceKind, InvoiceLine,
    };
    use crate::orders::{OrderDraft, OrderLine};
    use crate::stock_counts::{CountLine, StockCountDraft};
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::Category;

    const DATE: &str = "2026-01-01";

    fn sales_order(
        store: &mut Store,
        company: i64,
        customer: i64,
        item: i64,
        reference: &str,
    ) -> i64 {
        store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Sale,
                    party_ledger_id: customer,
                    reference: reference.into(),
                    date: DATE.into(),
                    expected_on: None,
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: item,
                        quantity: 8000,
                        unit_price: 1000,
                    }],
                },
            )
            .unwrap()
    }

    fn invoice(
        kind: InvoiceKind,
        party: i64,
        trade: i64,
        line: &StockNoteLineDraft,
    ) -> InvoiceDraft {
        InvoiceDraft {
            external_reference: Default::default(),
            kind,
            entry_mode: crate::inventory::InvoiceEntryMode::Invoice,
            date: DATE.into(),
            reference: "INV-01".into(),
            bill_reference: None,
            bill_splits: Vec::new(),
            narration: String::new(),
            due_date: None,
            credit_days: None,
            party_ledger_id: party,
            trade_ledger_id: trade,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: Vec::new(),
            tax_calculation: None,
            adjustments: Vec::new(),
            lines: vec![InvoiceLine {
                rate_basis: None,
                item_id: line.item_id,
                location_id: line.location_id,
                batch_id: line.batch_id,
                quantity: line.quantity,
                unit_price: line.unit_price,
                discount: InvoiceDiscount::None,
                trade_ledger_id: None,
                extra_allocations: Vec::new(),
            }],
        }
    }

    #[test]
    fn reservations_protect_their_buckets_and_follow_physical_fulfilment() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let trade = store
            .create_ledger(company, "Trade", Category::Income)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let other_location = store.create_stock_location(company, "Annex").unwrap();
        let batch = store
            .create_stock_batch(company, item, "Lot 1", None, None)
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                Some(batch),
                DATE,
                10_000,
                "Opening",
            )
            .unwrap();
        store
            .adjust_stock_batch(company, item, other_location, None, DATE, 3000, "Opening")
            .unwrap();
        let order = sales_order(&mut store, company, customer, item, "SO-01");
        let other = sales_order(&mut store, company, customer, item, "SO-02");
        let mut hold = StockReservationDraft {
            item_id: item,
            location_id: location,
            batch_id: Some(batch),
            quantity: 6000,
            reason: "Set aside for collection".into(),
        };
        let reservation = store.reserve_order_stock(company, order, &hold).unwrap();
        assert!(store.reserve_order_stock(company, order, &hold).is_err());
        assert!(store.reserve_order_stock(company, other, &hold).is_err());
        hold.quantity = 4000;
        store.reserve_order_stock(company, other, &hold).unwrap();
        assert!(store
            .release_order_stock(company, other, reservation, 1000, "Wrong order")
            .is_err());

        let mut delivery = StockNoteDraft {
            kind: InvoiceKind::Sale,
            date: DATE.into(),
            party_ledger_id: customer,
            order_id: None,
            reference: "DEL-01".into(),
            narration: String::new(),
            lines: vec![StockNoteLineDraft {
                item_id: item,
                location_id: location,
                batch_id: Some(batch),
                quantity: 2000,
                unit_price: 1000,
            }],
        };
        assert!(store.post_stock_note(company, &delivery).is_err());
        let mut direct = invoice(InvoiceKind::Sale, customer, trade, &delivery.lines[0]);
        assert!(store.create_invoice(company, &direct).is_err());
        assert!(store
            .transfer_stock_batch(
                company,
                item,
                Some(batch),
                location,
                other_location,
                DATE,
                1000,
                "Shift stock"
            )
            .is_err());
        let count = store
            .preview_stock_count(
                company,
                &StockCountDraft {
                    location_id: location,
                    date: DATE.into(),
                    reference: "COUNT-01".into(),
                    notes: String::new(),
                    lines: vec![CountLine {
                        item_id: item,
                        batch_id: Some(batch),
                        counted_quantity: 9000,
                    }],
                },
            )
            .unwrap();
        assert!(store.post_stock_count(company, &count).is_err());
        assert!(store.stock_counts(company).unwrap().is_empty());
        assert!(store
            .adjust_stock_batch(
                company,
                item,
                location,
                Some(batch),
                DATE,
                -1000,
                "Write down"
            )
            .is_err());

        delivery.order_id = Some(order);
        let delivery_id = store.post_stock_note(company, &delivery).unwrap();
        let allocations = store
            .order_detail(company, order)
            .unwrap()
            .reserved_allocations(item);
        assert_eq!(
            (allocations[0].batch_id, allocations[0].quantity),
            (Some(batch), 4000)
        );
        store
            .invoice_stock_note(company, delivery_id, &direct)
            .unwrap();
        let snapshot = store.order_availability(company).unwrap().remove(0);
        assert_eq!(
            (snapshot.on_hand, snapshot.reserved, snapshot.unreserved()),
            (11_000, 8000, 3000)
        );

        delivery.reference = "DEL-02".into();
        delivery.lines[0].location_id = other_location;
        delivery.lines[0].batch_id = None;
        delivery.lines[0].quantity = 3000;
        assert!(store.post_stock_note(company, &delivery).is_err());
        assert!(store
            .preclose_order_line(company, order, item, DATE, 3000, "Reduce")
            .is_err());
        direct.reference = "INV-02".into();
        direct.lines[0].extra_allocations.push(InvoiceAllocation {
            location_id: other_location,
            batch_id: None,
            quantity: 1000,
        });
        store.invoice_order(company, order, &direct).unwrap();
        store
            .release_order_stock(company, order, reservation, 1000, "Release one")
            .unwrap();
        let history = store
            .stock_reservation_history(company, order, reservation)
            .unwrap();
        assert_eq!(
            history
                .iter()
                .map(|row| (row.fulfilled, row.quantity))
                .collect::<Vec<_>>(),
            vec![(true, 2000), (true, 1000), (false, 1000)]
        );
        store.close_order(company, order, "Cancel balance").unwrap();
        assert_eq!(
            store.order_detail(company, order).unwrap().reservations[0].remaining,
            0
        );
        assert_eq!(store.order_availability(company).unwrap()[0].reserved, 4000);
        assert!(store
            .transfer_stock_batch(
                company,
                item,
                Some(batch),
                location,
                other_location,
                DATE,
                4000,
                "Shift past other hold"
            )
            .is_err());
        store
            .transfer_stock_batch(
                company,
                item,
                Some(batch),
                location,
                other_location,
                DATE,
                3000,
                "Shift free stock",
            )
            .unwrap();
    }

    #[test]
    fn reservations_account_for_future_movement_and_guard_purchase_changes() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let trade = store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let mut purchase = invoice(
            InvoiceKind::Purchase,
            supplier,
            trade,
            &StockNoteLineDraft {
                item_id: item,
                location_id: location,
                batch_id: None,
                quantity: 10_000,
                unit_price: 1000,
            },
        );
        let receipt = store.create_invoice(company, &purchase).unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2999-01-02",
                -5000,
                "Future issue",
            )
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2999-01-03",
                5000,
                "Future receipt",
            )
            .unwrap();
        let order = sales_order(&mut store, company, customer, item, "SO-01");
        let mut hold = StockReservationDraft {
            item_id: item,
            location_id: location,
            batch_id: None,
            quantity: 6000,
            reason: "Hold for collection".into(),
        };
        assert!(store.reserve_order_stock(company, order, &hold).is_err());
        hold.quantity = 5000;
        let reservation = store.reserve_order_stock(company, order, &hold).unwrap();
        purchase.lines[0].quantity = 9000;
        assert!(store.update_invoice(company, receipt, &purchase).is_err());
        assert_eq!(
            store.invoice_draft(company, receipt).unwrap().lines[0].quantity,
            10_000
        );
        assert!(store
            .reverse_invoice(company, receipt, DATE, "REV-01", "Cancel purchase")
            .is_err());
        store
            .release_order_stock(company, order, reservation, 5000, "Release hold")
            .unwrap();
        store
            .reverse_invoice(company, receipt, DATE, "REV-01", "Cancel purchase")
            .unwrap();
    }
}
