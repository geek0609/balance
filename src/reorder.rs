//! Current low-stock settings and company-wide quantity warnings.
use crate::{
    inventory::{check_quantity_precision, StockItem},
    Error, Result, Store,
};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorderRule {
    pub enabled: bool,
    pub level: i64,
    pub quantity: i64,
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
}
