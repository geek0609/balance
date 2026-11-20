//! Compare receipt acquisition costs with the net amounts billed by suppliers.

use crate::stock_notes::{invoice_quantity_share, InvoiceQuantityShare};
use crate::{parse_date, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct ReceiptReconciliationRow {
    pub note_id: i64,
    pub note_date: NaiveDate,
    pub reference: String,
    pub supplier: String,
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub received_quantity: i64,
    pub returned_quantity: i64,
    pub invoiced_quantity: i64,
    pub receipt_cost: Option<i128>,
    pub invoiced_receipt_cost: Option<i128>,
    pub invoice_net: i128,
}

impl ReceiptReconciliationRow {
    pub fn unbilled_quantity(&self) -> i64 {
        self.received_quantity - self.invoiced_quantity - self.returned_quantity
    }

    pub fn variance(&self) -> Option<i128> {
        self.invoiced_receipt_cost
            .map(|cost| self.invoice_net - cost)
    }
}

fn reconcile(
    conn: &Connection,
    company: i64,
    as_of: Option<NaiveDate>,
) -> Result<Vec<ReceiptReconciliationRow>> {
    let mut receipts = conn.prepare(
        "SELECT n.id, n.date, n.reference, p.name, nl.id, nl.quantity,
                s.name, s.unit, loc.name, b.name,
                (SELECT c.amount FROM stock_receipt_costs c
                 WHERE c.adjustment_id = nl.adjustment_id ORDER BY c.id DESC LIMIT 1),
                n.reversed_on
         FROM stock_notes n JOIN stock_note_lines nl ON nl.note_id = n.id
         JOIN ledgers p ON p.id = n.party_ledger_id AND p.company_id = n.company_id
         JOIN stock_items s ON s.id = nl.item_id AND s.company_id = n.company_id
         JOIN stock_locations loc ON loc.id = nl.location_id AND loc.company_id = n.company_id
         LEFT JOIN stock_batches b ON b.id = nl.batch_id AND b.company_id = n.company_id
         WHERE n.company_id = ?1 AND n.kind = 'Purchase'
         ORDER BY n.date DESC, n.id DESC, nl.id",
    )?;
    let mut invoices = conn.prepare(
        "SELECT l.id, l.quantity, l.amount, v.date, rv.date
         FROM stock_note_invoice_lines link JOIN invoice_lines l ON l.id = link.invoice_line_id
         JOIN invoices i ON i.id = l.invoice_id AND i.company_id = ?1
         JOIN vouchers v ON v.id = i.voucher_id AND v.company_id = i.company_id
         LEFT JOIN invoice_reversals r ON r.invoice_id = i.id AND r.company_id = i.company_id
         LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id AND rv.company_id = i.company_id
         WHERE link.note_line_id = ?2 ORDER BY v.date, l.id",
    )?;
    let is_effective = |date: NaiveDate, reversed: Option<&str>| -> Result<bool> {
        let reversed = reversed.map(parse_date).transpose()?;
        Ok(as_of.is_none_or(|end| date <= end)
            && reversed.is_none_or(|reversed| as_of.is_some_and(|end| reversed > end)))
    };
    let mut returns = conn.prepare(
        "SELECT id, quantity, net_amount, date, reversed_on FROM invoice_stock_return_history
         WHERE company_id = ?1 AND line_id = ?2",
    )?;
    let mut prior_bills = conn.prepare(
        "SELECT COALESCE(l.billed_quantity, l.quantity), l.amount,
                a.billed_offset + h.billed_start, h.note_start, h.quantity
         FROM invoice_stock_link_history h JOIN invoice_lines l ON l.id = h.line_id
         JOIN invoice_allocation_billing_offsets a ON a.allocation_id = h.invoice_allocation_id
         WHERE h.note_line_id = ?1 AND (?2 IS NULL OR h.linked_on <= ?2)
           AND (h.released_on IS NULL OR (?2 IS NOT NULL AND h.released_on > ?2))
           AND (h.invoice_reversed_on IS NULL OR (?2 IS NOT NULL AND h.invoice_reversed_on > ?2))",
    )?;
    let cutoff = as_of.map(|date| date.to_string());
    let mut rows = receipts.query([company])?;
    let mut report = Vec::new();
    while let Some(receipt) = rows.next()? {
        let date = parse_date(&receipt.get::<_, String>(1)?)?;
        let reversed = receipt.get::<_, Option<String>>(11)?;
        if !is_effective(date, reversed.as_deref())? {
            continue;
        }
        let note_line: i64 = receipt.get(4)?;
        let received_quantity: i64 = receipt.get(5)?;
        let mut returned_quantity = crate::stock_returns::returned_on(conn, note_line, as_of)?;
        let receipt_cost = receipt.get::<_, Option<i64>>(10)?.map(i128::from);
        let mut invoice_net = 0_i128;
        let mut quantity = 0_i128;
        let mut allocated_cost = 0_i128;
        let mut bills = invoices.query(rusqlite::params![company, note_line])?;
        while let Some(invoice) = bills.next()? {
            let date = parse_date(&invoice.get::<_, String>(3)?)?;
            let reversed = invoice.get::<_, Option<String>>(4)?;
            if !is_effective(date, reversed.as_deref())? {
                continue;
            }
            let billed: i64 = invoice.get(1)?;
            let share = invoice_quantity_share(conn, invoice.get(0)?, received_quantity, billed)?;
            if let Some(cost) = receipt_cost {
                allocated_cost += share.apportion(cost);
            }
            quantity += i128::from(billed);
            invoice_net += i128::from(invoice.get::<_, i64>(2)?);
            let line_id: i64 = invoice.get(0)?;
            let mut return_rows = returns.query(rusqlite::params![company, line_id])?;
            while let Some(returned) = return_rows.next()? {
                let date = parse_date(&returned.get::<_, String>(3)?)?;
                let reversed = returned.get::<_, Option<String>>(4)?;
                if !is_effective(date, reversed.as_deref())? {
                    continue;
                }
                let units: i64 = returned.get(1)?;
                quantity -= i128::from(units);
                returned_quantity = returned_quantity
                    .checked_add(units)
                    .ok_or_else(crate::stock_notes::invalid_spans)?;
                invoice_net -= i128::from(returned.get::<_, i64>(2)?);
                if let Some(cost) = receipt_cost {
                    let share = crate::invoice_returns::cost_share(
                        conn,
                        returned.get(0)?,
                        line_id,
                        billed,
                        units,
                        Some(received_quantity),
                    )?;
                    allocated_cost -= share.apportion(cost);
                }
            }
        }
        let mut prior_bill_rows = prior_bills.query(rusqlite::params![note_line, cutoff])?;
        while let Some(bill) = prior_bill_rows.next()? {
            let units = bill.get::<_, i64>(4)?;
            let billed_share = InvoiceQuantityShare::for_range(bill.get(0)?, bill.get(2)?, units)?;
            invoice_net += billed_share.apportion(i128::from(bill.get::<_, i64>(1)?));
            quantity += i128::from(units);
            if let Some(cost) = receipt_cost {
                let receipt_share =
                    InvoiceQuantityShare::for_range(received_quantity, bill.get(3)?, units)?;
                allocated_cost += receipt_share.apportion(cost);
            }
        }
        if quantity < 0
            || quantity + i128::from(returned_quantity) > i128::from(received_quantity)
            || invoice_net < 0
            || receipt_cost.is_some_and(|cost| !(0..=cost).contains(&allocated_cost))
        {
            return Err(Error::Validation(
                "Receipt and invoice quantities do not reconcile".into(),
            ));
        }
        report.push(ReceiptReconciliationRow {
            note_id: receipt.get(0)?,
            note_date: date,
            reference: receipt.get(2)?,
            supplier: receipt.get(3)?,
            item: receipt.get(6)?,
            unit: receipt.get(7)?,
            location: receipt.get(8)?,
            batch: receipt.get(9)?,
            received_quantity,
            returned_quantity,
            invoiced_quantity: quantity as i64,
            receipt_cost,
            invoiced_receipt_cost: receipt_cost.map(|_| allocated_cost),
            invoice_net,
        });
    }
    Ok(report)
}

impl Store {
    pub fn receipt_reconciliation(
        &self,
        company: i64,
        as_of: Option<&str>,
    ) -> Result<Vec<ReceiptReconciliationRow>> {
        let as_of = as_of.map(parse_date).transpose()?;
        let tx = self.conn.unchecked_transaction()?;
        let report = reconcile(&tx, company, as_of)?;
        tx.commit()?;
        Ok(report)
    }
}
