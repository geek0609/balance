//! Effective-dated, append-only stock price lists.
use crate::{parse_date, required, Error, Result, Store};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone)]
pub struct PriceList {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct StockPrice {
    pub item: String,
    pub unit: String,
    pub effective_date: String,
    pub unit_price: i64,
    pub reason: String,
    pub recorded_at: String,
}

impl Store {
    pub fn price_lists(&self, company: i64) -> Result<Vec<PriceList>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM companies WHERE id = ?1)",
            [company],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation("Company not found".into()));
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM price_lists WHERE company_id = ?1 ORDER BY name, id")?;
        let rows = stmt
            .query_map([company], |r| {
                Ok(PriceList {
                    id: r.get(0)?,
                    name: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn create_price_list(&self, company: i64, name: &str) -> Result<i64> {
        let name = required(name, "Price list name")?;
        let lists = self.price_lists(company)?;
        if lists
            .iter()
            .any(|list| list.name.trim().to_lowercase() == name.to_lowercase())
        {
            return Err(Error::Validation(
                "A price list with this name already exists".into(),
            ));
        }
        self.conn.execute(
            "INSERT INTO price_lists(company_id, name) VALUES (?1, ?2)",
            params![company, name],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn record_stock_price(
        &mut self,
        company: i64,
        list: i64,
        item: i64,
        date: &str,
        price: i64,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let reason = required(reason, "Price change reason")?;
        if price < 0 {
            return Err(Error::Validation("Price cannot be negative".into()));
        }
        let tx = self.conn.transaction()?;
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM price_lists p JOIN stock_items i ON i.company_id = p.company_id WHERE p.id = ?1 AND i.id = ?2 AND p.company_id = ?3)", params![list, item, company], |r| r.get(0))?;
        if !valid {
            return Err(Error::Validation(
                "Choose a price list and item in the selected company".into(),
            ));
        }
        tx.execute("INSERT INTO stock_prices(company_id, price_list_id, item_id, effective_date, unit_price, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![company, list, item, date, price, reason])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn stock_price(
        &self,
        company: i64,
        list: i64,
        item: i64,
        date: &str,
    ) -> Result<Option<i64>> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let valid: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM price_lists p JOIN stock_items i ON i.company_id = p.company_id WHERE p.id = ?1 AND i.id = ?2 AND p.company_id = ?3)", params![list, item, company], |r| r.get(0))?;
        if !valid {
            return Err(Error::Validation(
                "Choose a price list and item in the selected company".into(),
            ));
        }
        Ok(self.conn.query_row("SELECT unit_price FROM stock_prices WHERE company_id = ?1 AND price_list_id = ?2 AND item_id = ?3 AND effective_date <= ?4 ORDER BY effective_date DESC, id DESC LIMIT 1", params![company, list, item, date], |r| r.get(0)).optional()?)
    }

    pub fn stock_price_history(&self, company: i64, list: i64) -> Result<Vec<StockPrice>> {
        let valid: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM price_lists WHERE company_id = ?1 AND id = ?2)",
            params![company, list],
            |r| r.get(0),
        )?;
        if !valid {
            return Err(Error::Validation(
                "Price list not found in the selected company".into(),
            ));
        }
        let mut stmt = self.conn.prepare("SELECT i.name, i.unit, p.effective_date, p.unit_price, p.reason, p.created_at FROM stock_prices p JOIN stock_items i ON i.id = p.item_id AND i.company_id = p.company_id WHERE p.company_id = ?1 AND p.price_list_id = ?2 ORDER BY p.effective_date DESC, p.id DESC")?;
        let rows = stmt
            .query_map(params![company, list], |r| {
                Ok(StockPrice {
                    item: r.get(0)?,
                    unit: r.get(1)?,
                    effective_date: r.get(2)?,
                    unit_price: r.get(3)?,
                    reason: r.get(4)?,
                    recorded_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
