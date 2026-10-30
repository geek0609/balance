//! Stock quantities and basic sales/purchase invoices.
//! Quantities use thousandths; prices and accounting amounts use hundredths.
use crate::{
    check_ledgers, parse_date, required, validate_voucher, Category, Error, Posting, Result, Store,
    VoucherKind,
};
use rusqlite::{params, Connection};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use std::{collections::HashMap, str::FromStr};

#[derive(Debug, Clone)]
pub struct StockItem {
    pub id: i64,
    pub name: String,
    pub unit: String,
}
#[derive(Debug, Clone)]
pub struct StockBalance {
    pub item: StockItem,
    pub quantity: i128,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceKind {
    Sale,
    Purchase,
}
impl InvoiceKind {
    pub const ALL: [Self; 2] = [Self::Sale, Self::Purchase];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sale => "Sale",
            Self::Purchase => "Purchase",
        }
    }
    fn voucher_kind(self) -> VoucherKind {
        match self {
            Self::Sale => VoucherKind::Sales,
            Self::Purchase => VoucherKind::Purchase,
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "Sale" => Ok(Self::Sale),
            "Purchase" => Ok(Self::Purchase),
            _ => Err(Error::Validation("Unknown invoice type".into())),
        }
    }
}
#[derive(Debug, Clone)]
pub struct InvoiceLine {
    pub item_id: i64,
    pub quantity: i64,
    pub unit_price: i64,
}
#[derive(Debug, Clone)]
pub struct InvoiceDraft {
    pub kind: InvoiceKind,
    pub date: String,
    pub reference: String,
    pub narration: String,
    pub party_ledger_id: i64,
    pub trade_ledger_id: i64,
    pub tax_ledger_id: Option<i64>,
    pub tax_amount: i64,
    pub lines: Vec<InvoiceLine>,
}
#[derive(Debug, Clone)]
pub struct InvoiceSummary {
    pub id: i64,
    pub voucher_id: i64,
    pub kind: InvoiceKind,
    pub date: String,
    pub reference: String,
    pub party: String,
    pub total: i128,
    pub tax_amount: i64,
    pub lines: Vec<InvoiceDetailLine>,
}
#[derive(Debug, Clone)]
pub struct InvoiceDetailLine {
    pub item: String,
    pub unit: String,
    pub quantity: i64,
    pub unit_price: i64,
    pub amount: i64,
}

pub fn parse_quantity(input: &str) -> Result<i64> {
    let decimal = Decimal::from_str(input.trim())
        .map_err(|_| Error::Validation("Enter a valid quantity".into()))?;
    let scaled = decimal
        .checked_mul(Decimal::from(1000))
        .ok_or_else(|| Error::Validation("Quantity is too large".into()))?;
    if decimal < Decimal::ZERO || !scaled.fract().is_zero() {
        return Err(Error::Validation(
            "Quantities must be non-negative with at most three decimal places".into(),
        ));
    }
    scaled
        .to_i64()
        .ok_or_else(|| Error::Validation("Quantity is too large".into()))
}
pub fn format_quantity(quantity: i128) -> String {
    let n = quantity.unsigned_abs();
    format!(
        "{}{}.{:03}",
        if quantity < 0 { "-" } else { "" },
        n / 1000,
        n % 1000
    )
}
/// Multiply a positive quantity in thousandths by a unit price in hundredths,
/// rounding half up to one hundredth per line.
pub fn line_amount(quantity: i64, unit_price: i64) -> Result<i64> {
    if quantity <= 0 || unit_price < 0 {
        return Err(Error::Validation(
            "Each line needs a positive quantity and non-negative price".into(),
        ));
    }
    let amount = (i128::from(quantity) * i128::from(unit_price) + 500) / 1000;
    i64::try_from(amount).map_err(|_| Error::Validation("Invoice line amount is too large".into()))
}
fn ledger_category(conn: &Connection, company_id: i64, ledger_id: i64) -> Result<Category> {
    let value: String = conn
        .query_row(
            "SELECT category FROM ledgers WHERE id = ?1 AND company_id = ?2",
            params![ledger_id, company_id],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Validation("Ledger not found in the selected company".into())
            }
            other => Error::Database(other),
        })?;
    Category::parse(&value)
}
impl Store {
    pub fn create_stock_item(&self, company_id: i64, name: &str, unit: &str) -> Result<i64> {
        let name = required(name, "Stock item name")?;
        let unit = required(unit, "Unit")?;
        self.conn.execute(
            "INSERT INTO stock_items(company_id, name, unit) VALUES (?1, ?2, ?3)",
            params![company_id, name, unit],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn stock_items(&self, company_id: i64) -> Result<Vec<StockItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, unit FROM stock_items WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockItem {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    unit: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Quantity-only adjustment. It does not create an accounting entry.
    pub fn adjust_stock(
        &self,
        company_id: i64,
        item_id: i64,
        date: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let reason = required(reason, "Reason")?;
        if quantity == 0 {
            return Err(Error::Validation("Adjustment must be non-zero".into()));
        }
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
            params![item_id, company_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Stock item not found in the selected company".into(),
            ));
        }
        self.conn.execute("INSERT INTO stock_adjustments(company_id, item_id, date, quantity, reason) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, item_id, date, quantity, reason])?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn stock_balances(
        &self,
        company_id: i64,
        as_of: Option<&str>,
    ) -> Result<Vec<StockBalance>> {
        let end = as_of.map(parse_date).transpose()?;
        let items = self.stock_items(company_id)?;
        let mut sums = HashMap::<i64, i128>::new();
        let mut adjustments = self.conn.prepare(
            "SELECT item_id, date, quantity FROM stock_adjustments WHERE company_id = ?1",
        )?;
        let rows = adjustments.query_map([company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, date, quantity) = row?;
            let date = parse_date(&date)?;
            if end.is_none_or(|v| date <= v) {
                *sums.entry(id).or_default() += i128::from(quantity);
            }
        }
        let mut invoices = self.conn.prepare("SELECT l.item_id, v.date, i.kind, l.quantity FROM invoice_lines l JOIN invoices i ON i.id = l.invoice_id JOIN vouchers v ON v.id = i.voucher_id WHERE i.company_id = ?1")?;
        let rows = invoices.query_map([company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (id, date, kind, quantity) = row?;
            let date = parse_date(&date)?;
            if end.is_some_and(|v| date > v) {
                continue;
            }
            let sign = if InvoiceKind::parse(&kind)? == InvoiceKind::Purchase {
                1
            } else {
                -1
            };
            *sums.entry(id).or_default() += sign * i128::from(quantity);
        }
        Ok(items
            .into_iter()
            .map(|item| StockBalance {
                quantity: *sums.get(&item.id).unwrap_or(&0),
                item,
            })
            .collect())
    }
    /// Atomically creates the invoice, stock movements and balanced voucher.
    pub fn create_invoice(&mut self, company_id: i64, draft: &InvoiceDraft) -> Result<i64> {
        let date = parse_date(&draft.date)?.format("%Y-%m-%d").to_string();
        let reference = required(&draft.reference, "Invoice reference")?;
        if draft.lines.is_empty() {
            return Err(Error::Validation(
                "An invoice needs at least one item".into(),
            ));
        }
        if draft.tax_amount < 0
            || (draft.tax_amount > 0 && draft.tax_ledger_id.is_none())
            || (draft.tax_amount == 0 && draft.tax_ledger_id.is_some())
        {
            return Err(Error::Validation(
                "Choose a tax ledger exactly when a positive tax amount is entered".into(),
            ));
        }
        if draft.party_ledger_id == draft.trade_ledger_id
            || draft
                .tax_ledger_id
                .is_some_and(|id| id == draft.party_ledger_id || id == draft.trade_ledger_id)
        {
            return Err(Error::Validation(
                "Party, trade and tax ledgers must be different".into(),
            ));
        }
        let tx = self.conn.transaction()?;
        let party = ledger_category(&tx, company_id, draft.party_ledger_id)?;
        let trade = ledger_category(&tx, company_id, draft.trade_ledger_id)?;
        if !matches!(party, Category::Asset | Category::Liability) {
            return Err(Error::Validation(
                "The party ledger must be an asset or liability".into(),
            ));
        }
        if (draft.kind == InvoiceKind::Sale && trade != Category::Income)
            || (draft.kind == InvoiceKind::Purchase
                && !matches!(trade, Category::Expense | Category::Asset))
        {
            return Err(Error::Validation(
                "The trade ledger has an incompatible category".into(),
            ));
        }
        if let Some(id) = draft.tax_ledger_id {
            if !matches!(
                ledger_category(&tx, company_id, id)?,
                Category::Asset | Category::Liability
            ) {
                return Err(Error::Validation(
                    "The tax ledger must be an asset or liability".into(),
                ));
            }
        }
        let mut stmt = tx.prepare("SELECT v.reference FROM invoices i JOIN vouchers v ON v.id = i.voucher_id WHERE i.company_id = ?1 AND i.kind = ?2 AND i.party_ledger_id = ?3")?;
        let refs = stmt.query_map(
            params![company_id, draft.kind.as_str(), draft.party_ledger_id],
            |r| r.get::<_, String>(0),
        )?;
        for existing in refs {
            if existing?.trim().to_lowercase() == reference.to_lowercase() {
                return Err(Error::Validation(
                    "An invoice with this type, party and reference already exists".into(),
                ));
            }
        }
        drop(stmt);
        let mut amounts = Vec::with_capacity(draft.lines.len());
        let mut subtotal = 0_i128;
        for line in &draft.lines {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
                params![line.item_id, company_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "Every item must belong to the selected company".into(),
                ));
            }
            let amount = line_amount(line.quantity, line.unit_price)?;
            subtotal += i128::from(amount);
            amounts.push(amount);
        }
        let subtotal = i64::try_from(subtotal)
            .map_err(|_| Error::Validation("Invoice amount is too large".into()))?;
        let total = subtotal
            .checked_add(draft.tax_amount)
            .ok_or_else(|| Error::Validation("Invoice amount is too large".into()))?;
        if total == 0 {
            return Err(Error::Validation(
                "Invoice amount must be greater than zero".into(),
            ));
        }
        let direction = if draft.kind == InvoiceKind::Sale {
            1
        } else {
            -1
        };
        let mut postings = vec![Posting {
            ledger_id: draft.party_ledger_id,
            amount: direction * total,
        }];
        if subtotal != 0 {
            postings.push(Posting {
                ledger_id: draft.trade_ledger_id,
                amount: -direction * subtotal,
            });
        }
        if let Some(tax_id) = draft.tax_ledger_id {
            postings.push(Posting {
                ledger_id: tax_id,
                amount: -direction * draft.tax_amount,
            });
        }
        validate_voucher(&date, &postings)?;
        check_ledgers(&tx, company_id, &postings)?;
        tx.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, draft.kind.voucher_kind().as_str(), reference, date, draft.narration.trim()])?;
        let voucher_id = tx.last_insert_rowid();
        for p in &postings {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![voucher_id, p.ledger_id, p.amount],
            )?;
        }
        tx.execute("INSERT INTO invoices(company_id, voucher_id, kind, party_ledger_id, trade_ledger_id, tax_ledger_id, tax_amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![company_id, voucher_id, draft.kind.as_str(), draft.party_ledger_id, draft.trade_ledger_id, draft.tax_ledger_id, draft.tax_amount])?;
        let invoice_id = tx.last_insert_rowid();
        for (line, amount) in draft.lines.iter().zip(amounts) {
            tx.execute("INSERT INTO invoice_lines(invoice_id, item_id, quantity, unit_price, amount) VALUES (?1, ?2, ?3, ?4, ?5)", params![invoice_id, line.item_id, line.quantity, line.unit_price, amount])?;
        }
        tx.commit()?;
        Ok(invoice_id)
    }
    pub fn is_invoice_voucher(&self, company_id: i64, voucher_id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM invoices WHERE company_id = ?1 AND voucher_id = ?2)",
            params![company_id, voucher_id],
            |r| r.get(0),
        )?)
    }
    pub fn invoices(&self, company_id: i64) -> Result<Vec<InvoiceSummary>> {
        let mut stmt = self.conn.prepare("SELECT i.id, i.voucher_id, i.kind, v.date, v.reference, l.name, i.tax_amount FROM invoices i JOIN vouchers v ON v.id = i.voucher_id JOIN ledgers l ON l.id = i.party_ledger_id WHERE i.company_id = ?1 ORDER BY v.date DESC, i.id DESC")?;
        let rows = stmt.query_map([company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (id, voucher_id, kind, date, reference, party, tax) = row?;
            let mut amounts = self.conn.prepare("SELECT s.name, s.unit, l.quantity, l.unit_price, l.amount FROM invoice_lines l JOIN stock_items s ON s.id = l.item_id WHERE l.invoice_id = ?1 ORDER BY l.id")?;
            let line_amounts = amounts.query_map([id], |r| {
                Ok(InvoiceDetailLine {
                    item: r.get(0)?,
                    unit: r.get(1)?,
                    quantity: r.get(2)?,
                    unit_price: r.get(3)?,
                    amount: r.get(4)?,
                })
            })?;
            let mut total = i128::from(tax);
            let mut lines = Vec::new();
            for line in line_amounts {
                let line = line?;
                total += i128::from(line.amount);
                lines.push(line);
            }
            result.push(InvoiceSummary {
                id,
                voucher_id,
                kind: InvoiceKind::parse(&kind)?,
                date,
                reference,
                party,
                total,
                tax_amount: tax,
                lines,
            });
        }
        Ok(result)
    }
}
