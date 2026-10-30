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
    pub unit_id: i64,
    pub group_id: i64,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct StockItemRevision {
    pub changed_at: String,
    pub name: String,
    pub group: String,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct StockUnit {
    pub id: i64,
    pub symbol: String,
    pub formal_name: String,
    pub decimal_places: u8,
}
#[derive(Debug, Clone)]
pub struct StockGroup {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct StockBalance {
    pub item: StockItem,
    pub quantity: i128,
}
#[derive(Debug, Clone)]
pub struct StockMovement {
    pub date: String,
    pub description: String,
    pub quantity: i64,
    pub running_quantity: i128,
}
#[derive(Debug, Clone)]
pub struct StockRegister {
    pub item: StockItem,
    pub opening_quantity: i128,
    pub movements: Vec<StockMovement>,
    pub closing_quantity: i128,
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
    pub settled: i128,
    pub outstanding: i128,
    pub lines: Vec<InvoiceDetailLine>,
}
#[derive(Debug, Clone)]
pub struct Settlement {
    pub id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub amount: i64,
    pub bank_ledger: String,
}
#[derive(Debug, Clone)]
pub struct InvoiceRevision {
    pub changed_at: String,
    pub date: String,
    pub kind: InvoiceKind,
    pub reference: String,
    pub narration: String,
    pub party: String,
    pub trade: String,
    pub tax_ledger: Option<String>,
    pub tax_amount: i64,
    pub lines: Vec<InvoiceDetailLine>,
}
#[derive(Debug, Clone)]
pub struct OutstandingInvoice {
    pub invoice_id: i64,
    pub kind: InvoiceKind,
    pub date: String,
    pub reference: String,
    pub party: String,
    pub total: i128,
    pub settled: i128,
    pub outstanding: i128,
    /// Days since the invoice date, not days past a payment due date.
    pub age_days: i64,
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
struct PreparedInvoice {
    date: String,
    reference: String,
    postings: Vec<Posting>,
    amounts: Vec<i64>,
}
fn prepare_invoice(
    conn: &Connection,
    company_id: i64,
    draft: &InvoiceDraft,
    excluding: Option<i64>,
) -> Result<PreparedInvoice> {
    let date = parse_date(&draft.date)?.format("%Y-%m-%d").to_string();
    let reference = required(&draft.reference, "Invoice reference")?.to_owned();
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
    let party = ledger_category(conn, company_id, draft.party_ledger_id)?;
    let trade = ledger_category(conn, company_id, draft.trade_ledger_id)?;
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
            ledger_category(conn, company_id, id)?,
            Category::Asset | Category::Liability
        ) {
            return Err(Error::Validation(
                "The tax ledger must be an asset or liability".into(),
            ));
        }
    }
    let mut stmt = conn.prepare("SELECT v.reference FROM invoices i JOIN vouchers v ON v.id = i.voucher_id WHERE i.company_id = ?1 AND i.kind = ?2 AND i.party_ledger_id = ?3 AND (?4 IS NULL OR i.id != ?4)")?;
    for existing in stmt.query_map(
        params![
            company_id,
            draft.kind.as_str(),
            draft.party_ledger_id,
            excluding
        ],
        |r| r.get::<_, String>(0),
    )? {
        if existing?.trim().to_lowercase() == reference.to_lowercase() {
            return Err(Error::Validation(
                "An invoice with this type, party and reference already exists".into(),
            ));
        }
    }
    let mut amounts = Vec::with_capacity(draft.lines.len());
    let mut subtotal = 0_i128;
    for line in &draft.lines {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
            params![line.item_id, company_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Every item must belong to the selected company".into(),
            ));
        }
        check_quantity_precision(conn, company_id, line.item_id, line.quantity)?;
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
    check_ledgers(conn, company_id, &postings)?;
    crate::opening::check_no_control_postings(conn, company_id, &postings)?;
    Ok(PreparedInvoice {
        date,
        reference,
        postings,
        amounts,
    })
}
fn write_postings(conn: &Connection, voucher: i64, postings: &[Posting]) -> Result<()> {
    for posting in postings {
        conn.execute(
            "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
            params![voucher, posting.ledger_id, posting.amount],
        )?;
    }
    Ok(())
}
fn write_invoice_lines(
    conn: &Connection,
    invoice: i64,
    lines: &[InvoiceLine],
    amounts: &[i64],
) -> Result<()> {
    for (line, amount) in lines.iter().zip(amounts) {
        conn.execute("INSERT INTO invoice_lines(invoice_id, item_id, quantity, unit_price, amount) VALUES (?1, ?2, ?3, ?4, ?5)", params![invoice, line.item_id, line.quantity, line.unit_price, amount])?;
    }
    Ok(())
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
pub(crate) fn seed_stock_groups(conn: &Connection, company_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO stock_groups(company_id, name, parent_id) VALUES (?1, 'Primary', NULL)",
        [company_id],
    )?;
    Ok(())
}
pub(crate) fn check_quantity_precision(
    conn: &Connection,
    company_id: i64,
    item_id: i64,
    quantity: i64,
) -> Result<()> {
    let places: i64 = conn.query_row("SELECT u.decimal_places FROM stock_items i JOIN stock_units u ON u.id = i.unit_id AND u.company_id = i.company_id WHERE i.id = ?1 AND i.company_id = ?2", params![item_id, company_id], |r| r.get(0)).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Stock item or its unit was not found in the selected company".into()), other => Error::Database(other) })?;
    if !(0..=3).contains(&places) {
        return Err(Error::Validation("Unsupported unit precision".into()));
    }
    let step = 10_i64.pow((3 - places) as u32);
    if quantity % step != 0 {
        return Err(Error::Validation(format!(
            "Quantity must have at most {places} decimal places for this unit"
        )));
    }
    Ok(())
}
fn find_unit(conn: &Connection, company_id: i64, symbol: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id, symbol FROM stock_units WHERE company_id = ?1")?;
    let mut found = None;
    for row in stmt.query_map([company_id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        if name.trim().to_lowercase() == symbol.trim().to_lowercase() {
            if found.is_some() {
                return Err(Error::Validation("Unit name is ambiguous".into()));
            }
            found = Some(id);
        }
    }
    Ok(found)
}
pub(crate) fn create_stock_item_on_connection(
    conn: &Connection,
    company_id: i64,
    name: &str,
    unit_id: i64,
    group_id: i64,
    description: &str,
) -> Result<i64> {
    let name = required(name, "Stock item name")?;
    let unit: String = conn
        .query_row(
            "SELECT symbol FROM stock_units WHERE id = ?1 AND company_id = ?2",
            params![unit_id, company_id],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Validation("Unit not found in the selected company".into())
            }
            other => Error::Database(other),
        })?;
    let group_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stock_groups WHERE id = ?1 AND company_id = ?2)",
        params![group_id, company_id],
        |r| r.get(0),
    )?;
    if !group_exists {
        return Err(Error::Validation(
            "Stock group not found in the selected company".into(),
        ));
    }
    let mut stmt = conn.prepare("SELECT name FROM stock_items WHERE company_id = ?1")?;
    for existing in stmt.query_map([company_id], |r| r.get::<_, String>(0))? {
        if existing?.trim().to_lowercase() == name.to_lowercase() {
            return Err(Error::Validation("Stock item already exists".into()));
        }
    }
    drop(stmt);
    conn.execute("INSERT INTO stock_items(company_id, name, unit, unit_id, group_id, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![company_id, name, unit, unit_id, group_id, description.trim()])?;
    Ok(conn.last_insert_rowid())
}
impl Store {
    pub fn stock_units(&self, company_id: i64) -> Result<Vec<StockUnit>> {
        let mut stmt = self.conn.prepare("SELECT id, symbol, formal_name, decimal_places FROM stock_units WHERE company_id = ?1 ORDER BY symbol")?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockUnit {
                    id: r.get(0)?,
                    symbol: r.get(1)?,
                    formal_name: r.get(2)?,
                    decimal_places: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn create_stock_unit(
        &self,
        company_id: i64,
        symbol: &str,
        formal_name: &str,
        decimal_places: u8,
    ) -> Result<i64> {
        let symbol = required(symbol, "Unit symbol")?;
        if decimal_places > 3 {
            return Err(Error::Validation(
                "Units currently support at most three decimal places".into(),
            ));
        }
        if find_unit(&self.conn, company_id, symbol)?.is_some() {
            return Err(Error::Validation("Unit already exists".into()));
        }
        self.conn.execute("INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places) VALUES (?1, ?2, ?3, ?4)", params![company_id, symbol, formal_name.trim(), decimal_places])?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn stock_groups(&self, company_id: i64) -> Result<Vec<StockGroup>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id FROM stock_groups WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockGroup {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parent_id: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn create_stock_group(&self, company_id: i64, name: &str, parent_id: i64) -> Result<i64> {
        let name = required(name, "Stock group name")?;
        let groups = self.stock_groups(company_id)?;
        if !groups.iter().any(|g| g.id == parent_id) {
            return Err(Error::Validation(
                "Parent stock group not found in the selected company".into(),
            ));
        }
        if groups
            .iter()
            .any(|g| g.name.trim().to_lowercase() == name.to_lowercase())
        {
            return Err(Error::Validation("Stock group already exists".into()));
        }
        self.conn.execute(
            "INSERT INTO stock_groups(company_id, name, parent_id) VALUES (?1, ?2, ?3)",
            params![company_id, name, parent_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn create_stock_item_in_group(
        &self,
        company_id: i64,
        name: &str,
        unit_id: i64,
        group_id: i64,
    ) -> Result<i64> {
        create_stock_item_on_connection(&self.conn, company_id, name, unit_id, group_id, "")
    }
    pub fn create_stock_item_with_description(
        &self,
        company_id: i64,
        name: &str,
        unit_id: i64,
        group_id: i64,
        description: &str,
    ) -> Result<i64> {
        create_stock_item_on_connection(
            &self.conn,
            company_id,
            name,
            unit_id,
            group_id,
            description,
        )
    }
    pub fn create_stock_item(&self, company_id: i64, name: &str, unit: &str) -> Result<i64> {
        let name = required(name, "Stock item name")?;
        let unit = required(unit, "Unit")?;
        let tx = self.conn.unchecked_transaction()?;
        let unit_id = if let Some(id) = find_unit(&tx, company_id, unit)? {
            id
        } else {
            tx.execute("INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places) VALUES (?1, ?2, ?2, 3)", params![company_id, unit])?;
            tx.last_insert_rowid()
        };
        let group_id: i64 = tx.query_row("SELECT id FROM stock_groups WHERE company_id = ?1 AND name = 'Primary' AND parent_id IS NULL", [company_id], |r| r.get(0))?;
        let id = create_stock_item_on_connection(&tx, company_id, name, unit_id, group_id, "")?;
        tx.commit()?;
        Ok(id)
    }
    pub fn stock_items(&self, company_id: i64) -> Result<Vec<StockItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, unit, unit_id, group_id, description FROM stock_items WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockItem {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    unit: r.get(2)?,
                    unit_id: r.get(3)?,
                    group_id: r.get(4)?,
                    description: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Retain the previous name, group and description with each change.
    /// The base unit is deliberately immutable because existing quantities use it.
    pub fn update_stock_item(
        &mut self,
        company_id: i64,
        item_id: i64,
        name: &str,
        group_id: i64,
        description: &str,
    ) -> Result<()> {
        let name = required(name, "Stock item name")?;
        let tx = self.conn.transaction()?;
        let current: (String, i64, String) = tx.query_row(
            "SELECT name, group_id, description FROM stock_items WHERE id = ?1 AND company_id = ?2",
            params![item_id, company_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::Validation("Stock item not found in the selected company".into()),
            other => Error::Database(other),
        })?;
        let group_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_groups WHERE id = ?1 AND company_id = ?2)",
            params![group_id, company_id],
            |r| r.get(0),
        )?;
        if !group_exists {
            return Err(Error::Validation(
                "Stock group not found in the selected company".into(),
            ));
        }
        let mut stmt =
            tx.prepare("SELECT name FROM stock_items WHERE company_id = ?1 AND id != ?2")?;
        for existing in stmt.query_map(params![company_id, item_id], |r| r.get::<_, String>(0))? {
            if existing?.trim().to_lowercase() == name.to_lowercase() {
                return Err(Error::Validation("Stock item already exists".into()));
            }
        }
        drop(stmt);
        if current.0 != name || current.1 != group_id || current.2 != description.trim() {
            tx.execute("INSERT INTO stock_item_revisions(company_id, item_id, name, group_id, description) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, item_id, current.0, current.1, current.2])?;
            tx.execute("UPDATE stock_items SET name = ?1, group_id = ?2, description = ?3 WHERE id = ?4 AND company_id = ?5", params![name, group_id, description.trim(), item_id, company_id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn stock_item_revisions(
        &self,
        company_id: i64,
        item_id: i64,
    ) -> Result<Vec<StockItemRevision>> {
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
        let mut stmt = self.conn.prepare("SELECT r.changed_at, r.name, g.name, r.description FROM stock_item_revisions r JOIN stock_groups g ON g.id = r.group_id AND g.company_id = r.company_id WHERE r.company_id = ?1 AND r.item_id = ?2 ORDER BY r.id DESC")?;
        let rows = stmt
            .query_map(params![company_id, item_id], |r| {
                Ok(StockItemRevision {
                    changed_at: r.get(0)?,
                    name: r.get(1)?,
                    group: r.get(2)?,
                    description: r.get(3)?,
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
        check_quantity_precision(&self.conn, company_id, item_id, quantity)?;
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
    /// Quantity movements ordered by date; a date range includes both boundaries.
    pub fn stock_register(
        &self,
        company_id: i64,
        item_id: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<StockRegister> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(a), Some(b)) if a > b) {
            return Err(Error::Validation(
                "The start date must not be after the end date".into(),
            ));
        }
        let item = self
            .stock_items(company_id)?
            .into_iter()
            .find(|i| i.id == item_id)
            .ok_or_else(|| {
                Error::Validation("Stock item not found in the selected company".into())
            })?;
        let mut raw = Vec::<(chrono::NaiveDate, u8, i64, String, i64)>::new();
        let mut stmt = self.conn.prepare("SELECT id, date, quantity, reason FROM stock_adjustments WHERE company_id = ?1 AND item_id = ?2")?;
        let rows = stmt.query_map(params![company_id, item_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, date, qty, reason) = row?;
            raw.push((
                parse_date(&date)?,
                0,
                id,
                format!("Adjustment: {reason}"),
                qty,
            ));
        }
        let mut stmt = self.conn.prepare("SELECT l.id, v.date, i.kind, v.reference, l.quantity FROM invoice_lines l JOIN invoices i ON i.id = l.invoice_id JOIN vouchers v ON v.id = i.voucher_id WHERE i.company_id = ?1 AND l.item_id = ?2")?;
        let rows = stmt.query_map(params![company_id, item_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (id, date, kind, reference, qty) = row?;
            let kind = InvoiceKind::parse(&kind)?;
            raw.push((
                parse_date(&date)?,
                1,
                id,
                format!("{}: {}", kind.as_str(), reference),
                if kind == InvoiceKind::Purchase {
                    qty
                } else {
                    -qty
                },
            ));
        }
        raw.sort_by_key(|(date, kind, id, _, _)| (*date, *kind, *id));
        let mut opening = 0_i128;
        let mut running = 0_i128;
        let mut movements = Vec::new();
        for (date, _, _, description, quantity) in raw {
            if end.is_some_and(|v| date > v) {
                continue;
            }
            running += i128::from(quantity);
            if start.is_some_and(|v| date < v) {
                opening = running;
            } else {
                movements.push(StockMovement {
                    date: date.format("%Y-%m-%d").to_string(),
                    description,
                    quantity,
                    running_quantity: running,
                });
            }
        }
        Ok(StockRegister {
            item,
            opening_quantity: opening,
            movements,
            closing_quantity: running,
        })
    }
    /// Atomically creates the invoice, stock movements and balanced voucher.
    pub fn create_invoice(&mut self, company_id: i64, draft: &InvoiceDraft) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let prepared = prepare_invoice(&tx, company_id, draft, None)?;
        tx.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, draft.kind.voucher_kind().as_str(), prepared.reference, prepared.date, draft.narration.trim()])?;
        let voucher_id = tx.last_insert_rowid();
        write_postings(&tx, voucher_id, &prepared.postings)?;
        tx.execute("INSERT INTO invoices(company_id, voucher_id, kind, party_ledger_id, trade_ledger_id, tax_ledger_id, tax_amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![company_id, voucher_id, draft.kind.as_str(), draft.party_ledger_id, draft.trade_ledger_id, draft.tax_ledger_id, draft.tax_amount])?;
        let invoice_id = tx.last_insert_rowid();
        write_invoice_lines(&tx, invoice_id, &draft.lines, &prepared.amounts)?;
        tx.commit()?;
        Ok(invoice_id)
    }
    /// Return a complete editable draft, scoped to the company.
    pub fn invoice_draft(&self, company_id: i64, invoice_id: i64) -> Result<InvoiceDraft> {
        let (kind, date, reference, narration, party_ledger_id, trade_ledger_id, tax_ledger_id, tax_amount): (String, String, String, String, i64, i64, Option<i64>, i64) = self.conn.query_row("SELECT i.kind, v.date, v.reference, v.narration, i.party_ledger_id, i.trade_ledger_id, i.tax_ledger_id, i.tax_amount FROM invoices i JOIN vouchers v ON v.id = i.voucher_id WHERE i.id = ?1 AND i.company_id = ?2", params![invoice_id, company_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Invoice not found in the selected company".into()), other => Error::Database(other) })?;
        let mut stmt = self.conn.prepare("SELECT item_id, quantity, unit_price FROM invoice_lines WHERE invoice_id = ?1 ORDER BY id")?;
        let lines = stmt
            .query_map([invoice_id], |r| {
                Ok(InvoiceLine {
                    item_id: r.get(0)?,
                    quantity: r.get(1)?,
                    unit_price: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(InvoiceDraft {
            kind: InvoiceKind::parse(&kind)?,
            date,
            reference,
            narration,
            party_ledger_id,
            trade_ledger_id,
            tax_ledger_id,
            tax_amount,
            lines,
        })
    }
    /// Atomically replace an unsettled invoice, preserving both prior versions.
    pub fn update_invoice(
        &mut self,
        company_id: i64,
        invoice_id: i64,
        draft: &InvoiceDraft,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let voucher_id: i64 = tx
            .query_row(
                "SELECT voucher_id FROM invoices WHERE id = ?1 AND company_id = ?2",
                params![invoice_id, company_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Invoice not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        let settled: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM invoice_settlements WHERE company_id = ?1 AND invoice_id = ?2)", params![company_id, invoice_id], |r| r.get(0))?;
        if settled {
            return Err(Error::Validation(
                "An invoice with receipts or payments cannot yet be edited".into(),
            ));
        }
        let prepared = prepare_invoice(&tx, company_id, draft, Some(invoice_id))?;
        let saved = tx.execute("INSERT INTO voucher_revisions(voucher_id, company_id, date, narration, kind, reference) SELECT id, company_id, date, narration, kind, reference FROM vouchers WHERE id = ?1 AND company_id = ?2", params![voucher_id, company_id])?;
        if saved != 1 {
            return Err(Error::Validation("Associated voucher not found".into()));
        }
        let voucher_revision = tx.last_insert_rowid();
        tx.execute("INSERT INTO revision_postings(revision_id, ledger_id, amount) SELECT ?1, ledger_id, amount FROM postings WHERE voucher_id = ?2 ORDER BY id", params![voucher_revision, voucher_id])?;
        tx.execute("INSERT INTO invoice_revisions(company_id, invoice_id, voucher_revision_id, kind, party_ledger_id, trade_ledger_id, tax_ledger_id, tax_amount) SELECT company_id, id, ?1, kind, party_ledger_id, trade_ledger_id, tax_ledger_id, tax_amount FROM invoices WHERE id = ?2 AND company_id = ?3", params![voucher_revision, invoice_id, company_id])?;
        let revision_id = tx.last_insert_rowid();
        tx.execute("INSERT INTO invoice_revision_lines(revision_id, item_id, quantity, unit_price, amount) SELECT ?1, item_id, quantity, unit_price, amount FROM invoice_lines WHERE invoice_id = ?2 ORDER BY id", params![revision_id, invoice_id])?;
        tx.execute("UPDATE vouchers SET kind = ?1, reference = ?2, date = ?3, narration = ?4 WHERE id = ?5 AND company_id = ?6", params![draft.kind.voucher_kind().as_str(), prepared.reference, prepared.date, draft.narration.trim(), voucher_id, company_id])?;
        tx.execute("UPDATE invoices SET kind = ?1, party_ledger_id = ?2, trade_ledger_id = ?3, tax_ledger_id = ?4, tax_amount = ?5 WHERE id = ?6 AND company_id = ?7", params![draft.kind.as_str(), draft.party_ledger_id, draft.trade_ledger_id, draft.tax_ledger_id, draft.tax_amount, invoice_id, company_id])?;
        tx.execute("DELETE FROM postings WHERE voucher_id = ?1", [voucher_id])?;
        tx.execute(
            "DELETE FROM invoice_lines WHERE invoice_id = ?1",
            [invoice_id],
        )?;
        write_postings(&tx, voucher_id, &prepared.postings)?;
        write_invoice_lines(&tx, invoice_id, &draft.lines, &prepared.amounts)?;
        tx.commit()?;
        Ok(())
    }
    pub fn is_invoice_voucher(&self, company_id: i64, voucher_id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM invoices WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM invoice_settlements WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM opening_balances WHERE company_id = ?1 AND voucher_id = ?2)",
            params![company_id, voucher_id],
            |r| r.get(0),
        )?)
    }
    pub fn invoice_revisions(
        &self,
        company_id: i64,
        invoice_id: i64,
    ) -> Result<Vec<InvoiceRevision>> {
        let mut stmt = self.conn.prepare("SELECT r.id, r.changed_at, r.kind, v.date, v.reference, v.narration, p.name, t.name, x.name, r.tax_amount FROM invoice_revisions r JOIN voucher_revisions v ON v.id = r.voucher_revision_id JOIN ledgers p ON p.id = r.party_ledger_id JOIN ledgers t ON t.id = r.trade_ledger_id LEFT JOIN ledgers x ON x.id = r.tax_ledger_id WHERE r.company_id = ?1 AND r.invoice_id = ?2 ORDER BY r.id DESC")?;
        let rows = stmt
            .query_map(params![company_id, invoice_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut result = Vec::new();
        for (
            id,
            changed_at,
            kind,
            date,
            reference,
            narration,
            party,
            trade,
            tax_ledger,
            tax_amount,
        ) in rows
        {
            let mut items = self.conn.prepare("SELECT s.name, s.unit, l.quantity, l.unit_price, l.amount FROM invoice_revision_lines l JOIN stock_items s ON s.id = l.item_id WHERE l.revision_id = ?1 ORDER BY l.id")?;
            let lines = items
                .query_map([id], |r| {
                    Ok(InvoiceDetailLine {
                        item: r.get(0)?,
                        unit: r.get(1)?,
                        quantity: r.get(2)?,
                        unit_price: r.get(3)?,
                        amount: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            result.push(InvoiceRevision {
                changed_at,
                date,
                kind: InvoiceKind::parse(&kind)?,
                reference,
                narration,
                party,
                trade,
                tax_ledger,
                tax_amount,
                lines,
            });
        }
        Ok(result)
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
            let mut settlement_stmt = self.conn.prepare(
                "SELECT amount FROM invoice_settlements WHERE invoice_id = ?1 AND company_id = ?2",
            )?;
            let settlement_rows =
                settlement_stmt.query_map(params![id, company_id], |r| r.get::<_, i64>(0))?;
            let mut settled = 0_i128;
            for amount in settlement_rows {
                settled += i128::from(amount?);
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
                settled,
                outstanding: total - settled,
                lines,
            });
        }
        Ok(result)
    }
    /// Create and allocate a receipt or payment atomically to one invoice.
    pub fn settle_invoice(
        &mut self,
        company_id: i64,
        invoice_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        amount: i64,
    ) -> Result<i64> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let reference = required(reference, "Settlement reference")?;
        if amount <= 0 {
            return Err(Error::Validation(
                "Settlement amount must be positive".into(),
            ));
        }
        let tx = self.conn.transaction()?;
        let (kind, party, invoice_date, tax): (String, i64, String, i64) = tx.query_row("SELECT i.kind, i.party_ledger_id, v.date, i.tax_amount FROM invoices i JOIN vouchers v ON v.id = i.voucher_id WHERE i.id = ?1 AND i.company_id = ?2", params![invoice_id, company_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Invoice not found in the selected company".into()), other => Error::Database(other) })?;
        if parse_date(&date)? < parse_date(&invoice_date)? {
            return Err(Error::Validation(
                "Settlement date must not precede the invoice date".into(),
            ));
        }
        if bank_ledger_id == party
            || ledger_category(&tx, company_id, bank_ledger_id)? != Category::Asset
        {
            return Err(Error::Validation(
                "Select a different asset ledger for cash or bank".into(),
            ));
        }
        let kind = InvoiceKind::parse(&kind)?;
        let mut subtotal = i128::from(tax);
        let mut amounts = tx.prepare("SELECT amount FROM invoice_lines WHERE invoice_id = ?1")?;
        for value in amounts.query_map([invoice_id], |r| r.get::<_, i64>(0))? {
            subtotal += i128::from(value?);
        }
        drop(amounts);
        let mut paid = 0_i128;
        let mut previous = tx.prepare("SELECT s.amount, v.reference FROM invoice_settlements s JOIN vouchers v ON v.id = s.voucher_id WHERE s.invoice_id = ?1 AND s.company_id = ?2")?;
        for row in previous.query_map(params![invoice_id, company_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (value, old_reference) = row?;
            paid += i128::from(value);
            if old_reference.trim().to_lowercase() == reference.to_lowercase() {
                return Err(Error::Validation(
                    "Settlement reference already exists for this invoice".into(),
                ));
            }
        }
        drop(previous);
        if i128::from(amount) > subtotal - paid {
            return Err(Error::Validation(
                "Settlement exceeds the outstanding amount".into(),
            ));
        }
        let direction = if kind == InvoiceKind::Sale { 1 } else { -1 };
        let postings = [
            Posting {
                ledger_id: bank_ledger_id,
                amount: direction * amount,
            },
            Posting {
                ledger_id: party,
                amount: -direction * amount,
            },
        ];
        validate_voucher(&date, &postings)?;
        check_ledgers(&tx, company_id, &postings)?;
        let voucher_kind = if kind == InvoiceKind::Sale {
            VoucherKind::Receipt
        } else {
            VoucherKind::Payment
        };
        tx.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, voucher_kind.as_str(), reference, date, format!("Settlement for invoice {invoice_id}")])?;
        let voucher_id = tx.last_insert_rowid();
        for posting in postings {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![voucher_id, posting.ledger_id, posting.amount],
            )?;
        }
        tx.execute("INSERT INTO invoice_settlements(company_id, invoice_id, voucher_id, amount) VALUES (?1, ?2, ?3, ?4)", params![company_id, invoice_id, voucher_id, amount])?;
        let settlement_id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(settlement_id)
    }
    pub fn invoice_settlements(&self, company_id: i64, invoice_id: i64) -> Result<Vec<Settlement>> {
        let mut stmt = self.conn.prepare("SELECT s.id, s.voucher_id, v.date, v.reference, s.amount, l.name FROM invoice_settlements s JOIN vouchers v ON v.id = s.voucher_id JOIN postings p ON p.voucher_id = v.id JOIN ledgers l ON l.id = p.ledger_id JOIN invoices i ON i.id = s.invoice_id WHERE s.company_id = ?1 AND s.invoice_id = ?2 AND p.ledger_id != i.party_ledger_id ORDER BY v.date, s.id")?;
        let rows = stmt
            .query_map(params![company_id, invoice_id], |r| {
                Ok(Settlement {
                    id: r.get(0)?,
                    voucher_id: r.get(1)?,
                    date: r.get(2)?,
                    reference: r.get(3)?,
                    amount: r.get(4)?,
                    bank_ledger: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Unsettled invoices at the end of the requested date. Payments posted
    /// after that date do not affect the historical outstanding amount.
    pub fn outstanding_invoices(
        &self,
        company_id: i64,
        as_of: &str,
    ) -> Result<Vec<OutstandingInvoice>> {
        let end = parse_date(as_of)?;
        let mut result = Vec::new();
        for invoice in self.invoices(company_id)? {
            let date = parse_date(&invoice.date)?;
            if date > end {
                continue;
            }
            let mut settled = 0_i128;
            for settlement in self.invoice_settlements(company_id, invoice.id)? {
                if parse_date(&settlement.date)? <= end {
                    settled += i128::from(settlement.amount);
                }
            }
            let outstanding = invoice.total - settled;
            if outstanding < 0 {
                return Err(Error::Validation(
                    "Invoice settlements exceed its total".into(),
                ));
            }
            if outstanding == 0 {
                continue;
            }
            result.push(OutstandingInvoice {
                invoice_id: invoice.id,
                kind: invoice.kind,
                date: invoice.date,
                reference: invoice.reference,
                party: invoice.party,
                total: invoice.total,
                settled,
                outstanding,
                age_days: (end - date).num_days(),
            });
        }
        result.sort_by(|a, b| {
            a.party
                .to_lowercase()
                .cmp(&b.party.to_lowercase())
                .then_with(|| a.date.cmp(&b.date))
                .then_with(|| a.invoice_id.cmp(&b.invoice_id))
        });
        Ok(result)
    }
}
