//! Stock lines on sale invoices and their full reversals.
use crate::{parse_date, Result};
use chrono::NaiveDate;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct StockSale {
    pub line_id: i64,
    pub item_id: i64,
    pub original_date: NaiveDate,
    pub invoice_id: i64,
    pub voucher_id: i64,
    pub date: NaiveDate,
    pub reference: String,
    pub reversal: bool,
    pub reversed_same_day: bool,
    pub item: String,
    pub unit: String,
    pub quantity: i128,
    pub net_sales: i128,
}

pub(crate) fn stock_sales(
    conn: &Connection,
    company: i64,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Result<Vec<StockSale>> {
    let mut stmt = conn.prepare(
        "SELECT i.id, v.id, v.date, v.reference, s.id, s.name, s.unit, l.quantity, l.amount,
                rv.id, rv.date, rv.reference, l.id
         FROM invoices i JOIN invoice_lines l ON l.invoice_id = i.id
         JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
         JOIN stock_items s ON s.id = l.item_id AND s.company_id = i.company_id
         LEFT JOIN invoice_reversals r ON r.invoice_id = i.id AND r.company_id = i.company_id
         LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
         WHERE i.company_id = ?1 AND i.kind = 'Sale' ORDER BY v.id, l.id",
    )?;
    let mut rows = stmt.query([company])?;
    let mut sales = Vec::new();
    while let Some(row) = rows.next()? {
        let original_date = parse_date(&row.get::<_, String>(2)?)?;
        let reversal_date = row
            .get::<_, Option<String>>(10)?
            .as_deref()
            .map(parse_date)
            .transpose()?;
        let events: [Option<(i64, NaiveDate, String, i128)>; 2] = [
            Some((row.get(1)?, original_date, row.get(3)?, 1)),
            match reversal_date {
                Some(date) => Some((row.get(9)?, date, row.get(11)?, -1)),
                None => None,
            },
        ];
        for (voucher_id, date, reference, direction) in events.into_iter().flatten() {
            if start.is_some_and(|start| date < start) || end.is_some_and(|end| date > end) {
                continue;
            }
            sales.push(StockSale {
                line_id: row.get(12)?,
                item_id: row.get(4)?,
                original_date,
                invoice_id: row.get(0)?,
                voucher_id,
                date,
                reference,
                reversal: direction < 0,
                reversed_same_day: reversal_date == Some(original_date),
                item: row.get(5)?,
                unit: row.get(6)?,
                quantity: i128::from(row.get::<_, i64>(7)?) * direction,
                net_sales: i128::from(row.get::<_, i64>(8)?) * direction,
            });
        }
    }
    sales.sort_by_key(|sale| (sale.date, sale.voucher_id, sale.line_id));
    Ok(sales)
}
