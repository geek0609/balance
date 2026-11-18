//! Current low-stock settings and company-wide quantity warnings.
use crate::{
    inventory::{check_quantity_precision, StockItem},
    orders::OrderAvailability,
    Error, Result, Store,
};
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorderRule {
    pub enabled: bool,
    pub level: i64,
    pub quantity: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::InvoiceKind;
    use crate::orders::{OrderDraft, OrderLine};
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::stock_reservations::StockReservationDraft;
    use crate::Category;

    #[test]
    fn replenishment_uses_order_remainders_without_counting_reservations_twice() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let date = "2026-01-01";
        store
            .adjust_stock_batch(company, item, location, None, date, 8000, "Opening")
            .unwrap();
        store
            .save_reorder_rule(
                company,
                item,
                &ReorderRule {
                    enabled: true,
                    level: 3000,
                    quantity: 2000,
                },
            )
            .unwrap();
        let mut draft = OrderDraft {
            kind: InvoiceKind::Sale,
            party_ledger_id: customer,
            reference: "SELL-01".into(),
            date: date.into(),
            expected_on: None,
            notes: String::new(),
            lines: vec![OrderLine {
                item_id: item,
                quantity: 7000,
                unit_price: 1000,
            }],
        };
        let sale = store.create_order(company, &draft).unwrap();
        store
            .reserve_order_stock(
                company,
                sale,
                &StockReservationDraft {
                    item_id: item,
                    location_id: location,
                    batch_id: None,
                    quantity: 2000,
                    reason: "Hold".into(),
                },
            )
            .unwrap();
        let rows = store.replenishment_suggestions(company).unwrap();
        assert_eq!(
            (rows[0].stock.projected(), rows[0].suggested_quantity),
            (1000, 4000)
        );
        assert!(store.low_stock(company, None).unwrap().is_empty());
        draft.kind = InvoiceKind::Purchase;
        draft.party_ledger_id = supplier;
        draft.reference = "BUY-01".into();
        draft.lines[0].quantity = 4000;
        let purchase = store.create_order(company, &draft).unwrap();
        assert!(store.replenishment_suggestions(company).unwrap().is_empty());
        store
            .preclose_order_line(company, purchase, item, date, 3000, "Reduce incoming order")
            .unwrap();
        let rows = store.replenishment_suggestions(company).unwrap();
        assert_eq!(
            (rows[0].stock.projected(), rows[0].suggested_quantity),
            (2000, 2000)
        );
        store
            .post_stock_note(
                company,
                &StockNoteDraft {
                    kind: InvoiceKind::Purchase,
                    date: date.into(),
                    party_ledger_id: supplier,
                    order_id: Some(purchase),
                    reference: "RECEIPT-01".into(),
                    narration: String::new(),
                    lines: vec![StockNoteLineDraft {
                        item_id: item,
                        location_id: location,
                        batch_id: None,
                        quantity: 1000,
                        unit_price: 1000,
                    }],
                },
            )
            .unwrap();
        let rows = store.replenishment_suggestions(company).unwrap();
        assert_eq!(
            (
                rows[0].stock.on_hand,
                rows[0].stock.open_purchases,
                rows[0].stock.projected()
            ),
            (9000, 0, 2000)
        );
        store.close_order(company, sale, "Cancel order").unwrap();
        assert!(store.replenishment_suggestions(company).unwrap().is_empty());
    }
}
#[derive(Debug, Clone)]
pub struct ReorderRevision {
    pub changed_at: String,
    pub rule: ReorderRule,
}
#[derive(Debug, Clone)]
pub struct LowStock {
    pub item: StockItem,
    pub on_hand: i128,
    pub rule: ReorderRule,
}

#[derive(Debug, Clone)]
pub struct ReplenishmentSuggestion {
    pub stock: OrderAvailability,
    pub rule: ReorderRule,
    pub suggested_quantity: i128,
}

impl Store {
    pub fn reorder_rule(&self, company: i64, item: i64) -> Result<Option<ReorderRule>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_items WHERE company_id = ?1 AND id = ?2)",
            params![company, item],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Stock item not found in the selected company".into(),
            ));
        }
        Ok(self.conn.query_row("SELECT enabled, reorder_level, reorder_quantity FROM stock_reorder_rules WHERE company_id = ?1 AND item_id = ?2", params![company, item], |r| Ok(ReorderRule {enabled:r.get(0)?, level:r.get(1)?, quantity:r.get(2)?})).optional()?)
    }

    pub fn save_reorder_rule(&mut self, company: i64, item: i64, rule: &ReorderRule) -> Result<()> {
        if rule.level < 0 || rule.quantity <= 0 {
            return Err(Error::Validation(
                "Reorder level must be non-negative and reorder quantity positive".into(),
            ));
        }
        check_quantity_precision(&self.conn, company, item, rule.level)?;
        check_quantity_precision(&self.conn, company, item, rule.quantity)?;
        if self.reorder_rule(company, item)?.as_ref() == Some(rule) {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO stock_reorder_revisions(company_id, item_id, enabled, reorder_level, reorder_quantity) SELECT company_id, item_id, enabled, reorder_level, reorder_quantity FROM stock_reorder_rules WHERE company_id = ?1 AND item_id = ?2", params![company, item])?;
        tx.execute("INSERT INTO stock_reorder_rules(company_id, item_id, enabled, reorder_level, reorder_quantity) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(item_id) DO UPDATE SET enabled=excluded.enabled, reorder_level=excluded.reorder_level, reorder_quantity=excluded.reorder_quantity", params![company, item, rule.enabled, rule.level, rule.quantity])?;
        tx.commit()?;
        Ok(())
    }

    pub fn reorder_rule_history(&self, company: i64, item: i64) -> Result<Vec<ReorderRevision>> {
        self.reorder_rule(company, item)?;
        let mut stmt = self.conn.prepare("SELECT changed_at, enabled, reorder_level, reorder_quantity FROM stock_reorder_revisions WHERE company_id = ?1 AND item_id = ?2 ORDER BY id DESC")?;
        let rows = stmt
            .query_map(params![company, item], |r| {
                Ok(ReorderRevision {
                    changed_at: r.get(0)?,
                    rule: ReorderRule {
                        enabled: r.get(1)?,
                        level: r.get(2)?,
                        quantity: r.get(3)?,
                    },
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Uses present rules with quantities as of the requested date, across all locations.
    pub fn low_stock(&self, company: i64, as_of: Option<&str>) -> Result<Vec<LowStock>> {
        let mut result = Vec::new();
        for balance in self.stock_balances(company, as_of)? {
            if let Some(rule) = self.reorder_rule(company, balance.item.id)? {
                if rule.enabled && balance.quantity <= i128::from(rule.level) {
                    result.push(LowStock {
                        item: balance.item,
                        on_hand: balance.quantity,
                        rule,
                    });
                }
            }
        }
        Ok(result)
    }

    /// Plan from company-wide stock and open order quantities across all saved dates.
    pub fn replenishment_suggestions(&self, company: i64) -> Result<Vec<ReplenishmentSuggestion>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut rules = {
            let mut stmt = tx.prepare(
                "SELECT item_id, reorder_level, reorder_quantity FROM stock_reorder_rules
                 WHERE company_id = ?1 AND enabled = 1",
            )?;
            let rows = stmt.query_map([company], |r| {
                Ok((
                    r.get(0)?,
                    ReorderRule {
                        enabled: true,
                        level: r.get(1)?,
                        quantity: r.get(2)?,
                    },
                ))
            })?;
            rows.collect::<rusqlite::Result<HashMap<i64, ReorderRule>>>()?
        };
        let mut suggestions = Vec::new();
        if !rules.is_empty() {
            for stock in self.order_availability_in_snapshot(company)? {
                if let Some(rule) = rules.remove(&stock.item.id) {
                    let gap = i128::from(rule.level) - stock.projected();
                    if gap >= 0 {
                        let lot = i128::from(rule.quantity);
                        let suggested_quantity = (gap / lot + 1) * lot;
                        suggestions.push(ReplenishmentSuggestion {
                            stock,
                            rule,
                            suggested_quantity,
                        });
                    }
                }
            }
        }
        tx.commit()?;
        Ok(suggestions)
    }
}
