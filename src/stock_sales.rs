//! Stock lines on sale invoices, returns and reversals.
use crate::stock_notes::{invoice_quantity_share, InvoiceQuantityShare};
use crate::{parse_date, Result};
use chrono::NaiveDate;
use rusqlite::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SaleCostOrigin {
    Invoice(i64),
    Delivery(i64),
    ReturnCounter(i64),
}

#[derive(Debug, Clone, Copy)]
pub enum SalesStockDocument {
    Sale,
    Reversal,
    Return,
    ReturnReversal,
}

impl SalesStockDocument {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sale => "Sale",
            Self::Reversal => "Reversal",
            Self::Return => "Return",
            Self::ReturnReversal => "Return reversal",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StockSale {
    pub line_id: i64,
    pub item_id: i64,
    pub original_date: NaiveDate,
    pub(crate) cost_date: NaiveDate,
    pub(crate) cost_origin: SaleCostOrigin,
    pub(crate) counter_issue: Option<i64>,
    delivery_share: Option<InvoiceQuantityShare>,
    pub invoice_id: i64,
    pub voucher_id: i64,
    pub date: NaiveDate,
    pub reference: String,
    pub document: SalesStockDocument,
    pub reversal: bool,
    pub reversed_same_day: bool,
    pub item: String,
    pub unit: String,
    pub quantity: i128,
    pub net_sales: i128,
}

impl StockSale {
    pub(crate) fn issue_quantity(&self) -> i128 {
        self.delivery_share
            .as_ref()
            .map_or(self.quantity.abs(), |share| i128::from(share.quantity()))
    }

    pub(crate) fn allocated_issue_cost(&self, total: i128) -> i128 {
        let portion = self
            .delivery_share
            .as_ref()
            .map_or(total, |share| share.apportion(total));
        if self.reversal {
            -portion
        } else {
            portion
        }
    }
}

pub(crate) fn stock_sales(
    conn: &Connection,
    company: i64,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Result<Vec<StockSale>> {
    let mut stmt = conn.prepare(
        "SELECT i.id, v.id, v.date, v.reference, s.id, s.name, s.unit, l.quantity, l.amount,
                rv.id, rv.date, rv.reference, l.id, nl.note_line_id, note.date, delivered.quantity
         FROM invoices i JOIN invoice_lines l ON l.invoice_id = i.id
         JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
         JOIN stock_items s ON s.id = l.item_id AND s.company_id = i.company_id
         LEFT JOIN invoice_reversals r ON r.invoice_id = i.id AND r.company_id = i.company_id
         LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
         LEFT JOIN stock_note_invoice_lines nl ON nl.invoice_line_id = l.id
         LEFT JOIN stock_note_lines delivered ON delivered.id = nl.note_line_id
         LEFT JOIN stock_notes note ON note.id = delivered.note_id AND note.company_id = i.company_id
         WHERE i.company_id = ?1 AND i.kind = 'Sale' ORDER BY v.id, l.id",
    )?;
    let mut rows = stmt.query([company])?;
    let mut sales = Vec::new();
    let within_period =
        |date| start.is_none_or(|start| date >= start) && end.is_none_or(|end| date <= end);
    while let Some(row) = rows.next()? {
        let original_date = parse_date(&row.get::<_, String>(2)?)?;
        let reversal_date = row
            .get::<_, Option<String>>(10)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        if !within_period(original_date) && reversal_date.is_none_or(|date| !within_period(date)) {
            continue;
        }
        let line_id = row.get(12)?;
        let quantity: i64 = row.get(7)?;
        let cost_origin = row
            .get::<_, Option<i64>>(13)?
            .map_or(SaleCostOrigin::Invoice(line_id), SaleCostOrigin::Delivery);
        let cost_date = row
            .get::<_, Option<String>>(14)?
            .as_deref()
            .map(parse_date)
            .transpose()?
            .unwrap_or(original_date);
        let share = row
            .get::<_, Option<i64>>(15)?
            .map(|delivered| invoice_quantity_share(conn, line_id, delivered, quantity))
            .transpose()?;
        let events: [Option<(i64, NaiveDate, String, i128)>; 2] = [
            Some((row.get(1)?, original_date, row.get(3)?, 1)),
            match reversal_date {
                Some(date) => Some((row.get(9)?, date, row.get(11)?, -1)),
                None => None,
            },
        ];
        for (voucher_id, date, reference, direction) in events.into_iter().flatten() {
            if !within_period(date) {
                continue;
            }
            sales.push(StockSale {
                line_id,
                item_id: row.get(4)?,
                original_date,
                cost_date,
                cost_origin,
                counter_issue: None,
                delivery_share: share.clone(),
                invoice_id: row.get(0)?,
                voucher_id,
                date,
                reference,
                document: if direction < 0 {
                    SalesStockDocument::Reversal
                } else {
                    SalesStockDocument::Sale
                },
                reversal: direction < 0,
                reversed_same_day: reversal_date == Some(original_date),
                item: row.get(5)?,
                unit: row.get(6)?,
                quantity: i128::from(quantity) * direction,
                net_sales: i128::from(row.get::<_, i64>(8)?) * direction,
            });
        }
    }
    let mut stmt = conn.prepare(
        "SELECT r.id, r.invoice_id, r.line_id, r.quantity, r.net_amount, r.voucher_id,
                r.date, r.reference, r.reversal_voucher_id, r.reversed_on, r.reversal_reference,
                r.invoice_quantity, r.invoice_date, r.delivery_line, r.delivery_quantity, r.delivery_date,
                stock.id, stock.name, stock.unit, r.reversal_adjustment_id
         FROM invoice_stock_return_history r JOIN stock_items stock ON stock.id = r.item_id
         WHERE r.company_id = ?1 AND r.kind = 'Sale'",
    )?;
    let mut rows = stmt.query([company])?;
    while let Some(row) = rows.next()? {
        let returned = parse_date(&row.get::<_, String>(6)?)?;
        let reversed = row
            .get::<_, Option<String>>(9)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        if !within_period(returned) && reversed.is_none_or(|date| !within_period(date)) {
            continue;
        }
        let line_id = row.get(2)?;
        let quantity: i64 = row.get(3)?;
        let delivery = row.get::<_, Option<i64>>(13)?;
        let original_date = parse_date(&row.get::<_, String>(12)?)?;
        let cost_date = row
            .get::<_, Option<String>>(15)?
            .as_deref()
            .map(parse_date)
            .transpose()?
            .unwrap_or(original_date);
        let cost_origin =
            delivery.map_or(SaleCostOrigin::Invoice(line_id), SaleCostOrigin::Delivery);
        let share = crate::invoice_returns::cost_share(
            conn,
            row.get(0)?,
            line_id,
            row.get(11)?,
            quantity,
            row.get(14)?,
        )?;
        let events: [Option<(i64, NaiveDate, String, i128, Option<i64>)>; 2] = [
            Some((row.get(5)?, returned, row.get(7)?, -1, None)),
            match reversed {
                Some(date) => Some((row.get(8)?, date, row.get(10)?, 1, Some(row.get(19)?))),
                None => None,
            },
        ];
        for (voucher_id, date, reference, direction, counter_issue) in events.into_iter().flatten()
        {
            if !within_period(date) {
                continue;
            }
            sales.push(StockSale {
                line_id,
                item_id: row.get(16)?,
                original_date,
                cost_date,
                cost_origin,
                counter_issue,
                delivery_share: Some(share.clone()),
                invoice_id: row.get(1)?,
                voucher_id,
                date,
                reference,
                document: if direction < 0 {
                    SalesStockDocument::Return
                } else {
                    SalesStockDocument::ReturnReversal
                },
                reversal: direction < 0,
                reversed_same_day: reversed == Some(returned),
                item: row.get(17)?,
                unit: row.get(18)?,
                quantity: i128::from(quantity) * direction,
                net_sales: i128::from(row.get::<_, i64>(4)?) * direction,
            });
        }
    }
    sales.sort_by_key(|sale| (sale.date, sale.voucher_id, sale.line_id));
    Ok(sales)
}
