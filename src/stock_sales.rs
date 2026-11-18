//! Stock lines on sale invoices and their full reversals.
use crate::{parse_date, Error, Result};
use chrono::NaiveDate;
use rusqlite::Connection;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SaleCostOrigin {
    Invoice(i64),
    Delivery(i64),
}

#[derive(Debug, Clone)]
pub struct StockSale {
    pub line_id: i64,
    pub item_id: i64,
    pub original_date: NaiveDate,
    pub(crate) cost_date: NaiveDate,
    pub(crate) cost_origin: SaleCostOrigin,
    delivery_share: Option<DeliveryCostShare>,
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

#[derive(Debug, Clone)]
struct DeliveryCostShare {
    delivered: i64,
    spans: Vec<Range<i64>>,
}

impl StockSale {
    pub(crate) fn issue_quantity(&self) -> i128 {
        self.delivery_share
            .as_ref()
            .map_or(self.quantity.abs(), |share| i128::from(share.delivered))
    }

    pub(crate) fn allocated_issue_cost(&self, total: i128) -> i128 {
        let portion = self.delivery_share.as_ref().map_or(total, |share| {
            let delivered = i128::from(share.delivered);
            let boundary = |quantity: i64| {
                let position = i128::from(quantity);
                let scaled_remainder = (total % delivered) * position;
                let remainder = scaled_remainder % delivered;
                // Divide before multiplying to keep the full monetary range.
                // Differences of rounded endpoints conserve the whole cost.
                (total / delivered) * position
                    + scaled_remainder / delivered
                    + i128::from(remainder >= delivered - remainder)
            };
            share
                .spans
                .iter()
                .map(|span| boundary(span.end) - boundary(span.start))
                .sum()
        });
        if self.reversal {
            -portion
        } else {
            portion
        }
    }
}

fn delivery_share(
    conn: &Connection,
    invoice_line: i64,
    delivered: i64,
    billed: i64,
) -> Result<DeliveryCostShare> {
    let mismatch =
        || Error::Validation("Invoiced quantities do not reconcile with the delivery line".into());
    let mut stmt = conn.prepare_cached(
        "SELECT start_quantity, quantity FROM stock_note_invoice_spans
         WHERE invoice_line_id = ?1 ORDER BY start_quantity",
    )?;
    let mut spans = Vec::new();
    let mut previous_end = 0;
    let mut subtotal = 0;
    for row in stmt.query_map([invoice_line], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (start, quantity) = row?;
        let end = start.checked_add(quantity).ok_or_else(mismatch)?;
        if start < previous_end || quantity <= 0 || end > delivered {
            return Err(mismatch());
        }
        spans.push(start..end);
        previous_end = end;
        subtotal += quantity;
    }
    if subtotal != billed || spans.is_empty() {
        return Err(mismatch());
    }
    Ok(DeliveryCostShare { delivered, spans })
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
         LEFT JOIN stock_note_invoices sn ON sn.invoice_id = i.id
         LEFT JOIN stock_notes note ON note.id = sn.note_id AND note.company_id = i.company_id
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
            .map(|delivered| delivery_share(conn, line_id, delivered, quantity))
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
                delivery_share: share.clone(),
                invoice_id: row.get(0)?,
                voucher_id,
                date,
                reference,
                reversal: direction < 0,
                reversed_same_day: reversal_date == Some(original_date),
                item: row.get(5)?,
                unit: row.get(6)?,
                quantity: i128::from(quantity) * direction,
                net_sales: i128::from(row.get::<_, i64>(8)?) * direction,
            });
        }
    }
    sales.sort_by_key(|sale| (sale.date, sale.voucher_id, sale.line_id));
    Ok(sales)
}
