//! Preview and reserve available stock against pending sales orders.
use crate::inventory::{
    check_quantity_precision, check_stock_batch, check_stock_location, stock_available_to_reserve,
};
use crate::orders::remaining_quantity;
use crate::stock_reservations::{
    check_stock_coverage, insert_reservation, reserved_in_bucket, StockReservationDraft,
};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, TransactionBehavior};

#[derive(Debug, Clone)]
pub struct StockAllocationDraft {
    pub item_id: i64,
    pub location_id: i64,
    pub batch_id: Option<i64>,
    pub maximum: Option<i64>,
    pub expected_through: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockAllocation {
    pub order_id: i64,
    pub reference: String,
    pub party: String,
    pub order_date: NaiveDate,
    pub expected_on: Option<NaiveDate>,
    pub unreserved: i64,
    pub quantity: i64,
}

#[derive(Debug, Clone)]
pub struct StockAllocationPlan {
    company: i64,
    draft: StockAllocationDraft,
    available: i128,
    allocations: Vec<StockAllocation>,
}

impl StockAllocationPlan {
    pub fn available(&self) -> i128 {
        self.available
    }

    pub fn allocations(&self) -> &[StockAllocation] {
        &self.allocations
    }
}

fn preview(
    conn: &Connection,
    company: i64,
    draft: &StockAllocationDraft,
) -> Result<StockAllocationPlan> {
    let through = draft
        .expected_through
        .as_deref()
        .map(parse_date)
        .transpose()?;
    if draft.maximum.is_some_and(|quantity| quantity <= 0) {
        return Err(Error::Validation(
            "Enter a positive allocation limit".into(),
        ));
    }
    check_quantity_precision(conn, company, draft.item_id, draft.maximum.unwrap_or(0))?;
    check_stock_location(conn, company, draft.location_id)?;
    check_stock_batch(conn, company, draft.item_id, draft.batch_id)?;
    let available = (stock_available_to_reserve(
        conn,
        company,
        draft.item_id,
        draft.location_id,
        draft.batch_id,
    )? - reserved_in_bucket(
        conn,
        company,
        draft.item_id,
        draft.location_id,
        draft.batch_id,
    )?)
    .max(0);
    let mut budget = draft
        .maximum
        .map_or(available, |limit| available.min(i128::from(limit)));
    let mut candidates = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT o.id, o.reference, l.name, o.date, o.expected_on,
                p.quantity, p.fulfilled, p.preclosed, COALESCE((
                    SELECT SUM(r.remaining) FROM stock_reservation_balances r WHERE r.order_line_id = p.id
                ), 0)
         FROM order_line_availability p JOIN orders o ON o.id = p.order_id
         JOIN ledgers l ON l.company_id = o.company_id AND l.id = o.party_ledger_id
         WHERE o.company_id = ?1 AND p.item_id = ?2 AND o.kind = 'Sale' AND o.closed_at IS NULL",
    )?;
    let mut rows = stmt.query(params![company, draft.item_id])?;
    while let Some(row) = rows.next()? {
        let expected_on: Option<String> = row.get(4)?;
        let expected_on = expected_on.as_deref().map(parse_date).transpose()?;
        if through.is_some_and(|limit| expected_on.is_none_or(|date| date > limit)) {
            continue;
        }
        let pending = remaining_quantity(row.get(5)?, row.get(6)?, row.get(7)?)?;
        let reserved: i64 = row.get(8)?;
        if !(0..=pending).contains(&reserved) {
            return Err(Error::Validation(
                "Order reservations do not match the pending quantities".into(),
            ));
        }
        if reserved < pending {
            candidates.push(StockAllocation {
                order_id: row.get(0)?,
                reference: row.get(1)?,
                party: row.get(2)?,
                order_date: parse_date(&row.get::<_, String>(3)?)?,
                expected_on,
                unreserved: pending - reserved,
                quantity: 0,
            });
        }
    }
    candidates.sort_by_key(|row| {
        (
            row.expected_on.is_none(),
            row.expected_on,
            row.order_date,
            row.order_id,
        )
    });
    let mut allocations = Vec::new();
    for mut row in candidates {
        if budget == 0 {
            break;
        }
        row.quantity = budget.min(i128::from(row.unreserved)) as i64;
        budget -= i128::from(row.quantity);
        allocations.push(row);
    }
    Ok(StockAllocationPlan {
        company,
        draft: draft.clone(),
        available,
        allocations,
    })
}

impl Store {
    pub fn preview_stock_allocation(
        &self,
        company: i64,
        draft: &StockAllocationDraft,
    ) -> Result<StockAllocationPlan> {
        let tx = self.conn.unchecked_transaction()?;
        let plan = preview(&tx, company, draft)?;
        tx.commit()?;
        Ok(plan)
    }

    pub fn apply_stock_allocation(
        &mut self,
        company: i64,
        plan: &StockAllocationPlan,
        reason: &str,
    ) -> Result<usize> {
        let reason = required(reason, "Allocation reason")?;
        if company != plan.company || plan.allocations.is_empty() {
            return Err(Error::Validation(
                "Preview an allocation for this company first".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = preview(&tx, company, &plan.draft)?;
        if current.allocations != plan.allocations {
            return Err(Error::Validation(
                "The proposed reservations changed. Preview the allocation again.".into(),
            ));
        }
        let mut reservation = StockReservationDraft {
            item_id: plan.draft.item_id,
            location_id: plan.draft.location_id,
            batch_id: plan.draft.batch_id,
            quantity: 0,
            reason: reason.to_owned(),
        };
        for row in &current.allocations {
            reservation.quantity = row.quantity;
            insert_reservation(&tx, company, row.order_id, &reservation)?;
        }
        check_stock_coverage(
            &tx,
            company,
            reservation.item_id,
            reservation.location_id,
            reservation.batch_id,
        )?;
        tx.commit()?;
        Ok(current.allocations.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::InvoiceKind;
    use crate::orders::{OrderDraft, OrderLine};
    use crate::Category;

    #[test]
    fn allocation_revalidates_priority_and_stock_before_reserving() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let other = store.create_company("Other").unwrap();
        let party = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let annex = store.create_stock_location(company, "Annex").unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2026-01-01",
                11_000,
                "Opening",
            )
            .unwrap();
        store
            .adjust_stock_batch(company, item, annex, None, "2026-01-01", 2000, "Opening")
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2999-01-02",
                -3000,
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
                3000,
                "Future receipt",
            )
            .unwrap();
        let create = |store: &mut Store,
                      reference: &str,
                      date: &str,
                      expected_on: Option<&str>,
                      quantity| {
            store
                .create_order(
                    company,
                    &OrderDraft {
                        kind: InvoiceKind::Sale,
                        party_ledger_id: party,
                        reference: reference.into(),
                        date: date.into(),
                        expected_on: expected_on.map(str::to_owned),
                        notes: String::new(),
                        lines: vec![OrderLine {
                            item_id: item,
                            quantity,
                            unit_price: 1000,
                        }],
                    },
                )
                .unwrap()
        };
        let undated = create(&mut store, "UNDATED", "2026-01-01", None, 4000);
        let second = create(&mut store, "SECOND", "2026-01-01", Some("2026-01-20"), 6000);
        let first = create(&mut store, "FIRST", "2026-01-02", Some("2026-01-15"), 4000);
        let hold = |location_id| StockReservationDraft {
            item_id: item,
            location_id,
            batch_id: None,
            quantity: 1000,
            reason: "Manual hold".into(),
        };
        store
            .reserve_order_stock(company, first, &hold(annex))
            .unwrap();
        store
            .reserve_order_stock(company, second, &hold(location))
            .unwrap();
        let mut draft = StockAllocationDraft {
            item_id: item,
            location_id: location,
            batch_id: None,
            maximum: Some(5000),
            expected_through: None,
        };
        let quantities = |plan: &StockAllocationPlan| {
            plan.allocations()
                .iter()
                .map(|row| (row.order_id, row.quantity))
                .collect::<Vec<_>>()
        };
        let plan = store.preview_stock_allocation(company, &draft).unwrap();
        assert_eq!(plan.available(), 7000);
        assert_eq!(quantities(&plan), vec![(first, 3000), (second, 2000)]);
        assert!(store
            .apply_stock_allocation(other, &plan, "Other company")
            .is_err());
        store
            .preclose_order_line(
                company,
                first,
                item,
                "2026-01-03",
                1000,
                "Reduce first line",
            )
            .unwrap();
        assert!(store
            .apply_stock_allocation(company, &plan, "Stale plan")
            .is_err());
        assert_eq!(
            store
                .order_detail(company, second)
                .unwrap()
                .reservations
                .len(),
            1
        );
        draft.expected_through = Some("2026-01-15".into());
        let filtered = store.preview_stock_allocation(company, &draft).unwrap();
        assert_eq!(quantities(&filtered), vec![(first, 2000)]);
        draft.maximum = None;
        draft.expected_through = None;
        let plan = store.preview_stock_allocation(company, &draft).unwrap();
        assert_eq!(quantities(&plan), vec![(first, 2000), (second, 5000)]);
        assert_eq!(
            store
                .apply_stock_allocation(company, &plan, "Allocate available stock")
                .unwrap(),
            2
        );
        assert!(store
            .apply_stock_allocation(company, &plan, "Duplicate plan")
            .is_err());
        assert!(store
            .order_detail(company, undated)
            .unwrap()
            .reservations
            .is_empty());
        let saved = store.order_detail(company, first).unwrap();
        assert_eq!(saved.reservations[1].reason, "Allocate available stock");
        assert_eq!(saved.reservations[1].remaining, 2000);
        assert_eq!(
            store
                .preview_stock_allocation(company, &draft)
                .unwrap()
                .available(),
            0
        );
    }
}
