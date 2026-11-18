//! Stock movements and net item credits against invoiced quantities.

use crate::inventory::{check_quantity_precision, InvoiceKind};
use crate::invoice_credits::CreditLineDraft;
use crate::stock_notes::{self, InvoiceQuantityShare, StockNoteLineDraft};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE invoice_return_lines(
            id INTEGER PRIMARY KEY,
            note_id INTEGER NOT NULL REFERENCES invoice_credit_notes(id),
            allocation_id INTEGER NOT NULL REFERENCES invoice_line_allocations(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            net_amount INTEGER NOT NULL CHECK(net_amount >= 0),
            adjustment_id INTEGER NOT NULL UNIQUE REFERENCES stock_adjustments(id),
            reversal_adjustment_id INTEGER UNIQUE REFERENCES stock_adjustments(id),
            UNIQUE(note_id, allocation_id)
        );
        CREATE INDEX invoice_return_lines_allocation ON invoice_return_lines(allocation_id);
        CREATE TABLE invoice_return_spans(
            return_line_id INTEGER NOT NULL REFERENCES invoice_return_lines(id),
            start_quantity INTEGER NOT NULL CHECK(start_quantity >= 0),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            PRIMARY KEY(return_line_id, start_quantity)
        );
        CREATE VIEW invoice_stock_return_history AS
            SELECT ret.id, ret.note_id, n.company_id, n.invoice_id, i.kind, a.line_id,
                   a.location_id, a.batch_id, l.item_id, ret.quantity, ret.net_amount, ret.adjustment_id,
                   ret.reversal_adjustment_id, v.date, v.id AS voucher_id, v.reference,
                   rv.date AS reversed_on, rv.id AS reversal_voucher_id, rv.reference AS reversal_reference,
                   l.quantity AS invoice_quantity, iv.date AS invoice_date, src.id AS delivery_line,
                   src.quantity AS delivery_quantity, src.adjustment_id AS delivery_adjustment_id,
                   physical.date AS delivery_date
            FROM invoice_return_lines ret JOIN invoice_credit_notes n ON n.id = ret.note_id
            JOIN invoice_line_allocations a ON a.id = ret.allocation_id
            JOIN invoice_lines l ON l.id = a.line_id AND l.invoice_id = n.invoice_id
            JOIN invoices i ON i.id = l.invoice_id AND i.company_id = n.company_id
            JOIN vouchers v ON v.id = n.voucher_id AND v.company_id = n.company_id
            JOIN vouchers iv ON iv.id = i.voucher_id
            LEFT JOIN vouchers rv ON rv.id = n.reversal_voucher_id AND rv.company_id = n.company_id
            LEFT JOIN stock_note_invoice_lines link ON link.invoice_line_id = l.id
            LEFT JOIN stock_note_lines src ON src.id = link.note_line_id
            LEFT JOIN stock_notes physical ON physical.id = src.note_id AND physical.company_id = n.company_id;
        CREATE VIEW invoice_returned_quantities AS
            SELECT line_id, delivery_line, SUM(quantity) AS returned
            FROM invoice_stock_return_history WHERE reversed_on IS NULL GROUP BY line_id;
        DROP VIEW order_line_progress;
        CREATE VIEW order_line_progress AS
            SELECT ol.id, ol.order_id, ol.item_id, ol.quantity, ol.unit_price,
                COALESCE((
                    SELECT SUM(il.quantity - COALESCE(rq.returned, 0))
                    FROM invoice_order_links link JOIN physical_invoice_lines il ON il.invoice_id = link.invoice_id
                    LEFT JOIN invoice_returned_quantities rq ON rq.line_id = il.id
                    WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                      AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                ), 0) + COALESCE((
                    SELECT SUM(nl.quantity - COALESCE(r.returned, 0) - COALESCE((
                        SELECT SUM(rq.returned) FROM invoice_returned_quantities rq WHERE rq.delivery_line = nl.id
                    ), 0))
                    FROM stock_notes n JOIN stock_note_lines nl ON nl.note_id = n.id
                    LEFT JOIN stock_note_returned_quantities r ON r.note_line_id = nl.id
                    WHERE n.order_id = ol.order_id AND nl.item_id = ol.item_id AND n.reversed_on IS NULL
                ), 0) AS fulfilled,
                COALESCE((
                    SELECT SUM(il.quantity - COALESCE(rq.returned, 0))
                    FROM invoice_order_links link JOIN invoice_lines il ON il.invoice_id = link.invoice_id
                    LEFT JOIN invoice_returned_quantities rq ON rq.line_id = il.id
                    WHERE link.order_id = ol.order_id AND il.item_id = ol.item_id
                      AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = link.invoice_id)
                ), 0) AS billed
            FROM order_lines ol;",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ReturnAllocation {
    pub id: i64,
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub remaining: i64,
}

#[derive(Debug, Clone)]
pub struct ReturnLineDraft {
    pub allocation: i64,
    pub quantity: i64,
}

struct Allocation {
    available: ReturnAllocation,
    stock: StockNoteLineDraft,
    line_quantity: i64,
    offset: i64,
    line_net: i64,
    ledger: i64,
}

fn allocations(conn: &Connection, company: i64, invoice: i64) -> Result<Vec<Allocation>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, item.name, item.unit, loc.name, batch.name, a.quantity, l.quantity,
                COALESCE(SUM(a.quantity) OVER(PARTITION BY l.id ORDER BY a.id
                    ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING), 0),
                l.amount, COALESCE(l.trade_ledger_id, i.trade_ledger_id), l.item_id, a.location_id, a.batch_id,
                COALESCE((SELECT SUM(ret.quantity) FROM invoice_return_lines ret
                    JOIN invoice_credit_notes n ON n.id = ret.note_id AND n.reversal_voucher_id IS NULL
                    WHERE ret.allocation_id = a.id), 0)
         FROM invoices i JOIN invoice_lines l ON l.invoice_id = i.id
         JOIN invoice_line_allocations a ON a.line_id = l.id
         JOIN stock_items item ON item.id = l.item_id AND item.company_id = i.company_id
         JOIN stock_locations loc ON loc.id = a.location_id AND loc.company_id = i.company_id
         LEFT JOIN stock_batches batch ON batch.id = a.batch_id
         WHERE i.company_id = ?1 AND i.id = ?2 ORDER BY l.id, a.id",
    )?;
    let rows = stmt
        .query_map(params![company, invoice], |row| {
            let quantity: i64 = row.get(5)?;
            let returned: i64 = row.get(13)?;
            Ok(Allocation {
                available: ReturnAllocation {
                    id: row.get(0)?,
                    item: row.get(1)?,
                    unit: row.get(2)?,
                    location: row.get(3)?,
                    batch: row.get(4)?,
                    remaining: quantity - returned,
                },
                stock: StockNoteLineDraft {
                    item_id: row.get(10)?,
                    location_id: row.get(11)?,
                    batch_id: row.get(12)?,
                    quantity,
                    unit_price: 0,
                },
                line_quantity: row.get(6)?,
                offset: row.get(7)?,
                line_net: row.get(8)?,
                ledger: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

struct ReturnPlan {
    allocation: Allocation,
    share: InvoiceQuantityShare,
    net_amount: i64,
}

fn plan(
    conn: &Connection,
    company: i64,
    invoice: i64,
    lines: &[ReturnLineDraft],
) -> Result<Vec<ReturnPlan>> {
    if lines.is_empty() {
        return Err(Error::Validation("Enter a quantity to return".into()));
    }
    let available = allocations(conn, company, invoice)?;
    let mut seen = HashSet::new();
    let mut planned = Vec::new();
    let mut earlier = conn.prepare_cached(
        "SELECT s.start_quantity - ?2, s.quantity FROM invoice_return_spans s
         JOIN invoice_return_lines ret ON ret.id = s.return_line_id
         JOIN invoice_credit_notes n ON n.id = ret.note_id AND n.reversal_voucher_id IS NULL
         WHERE ret.allocation_id = ?1 ORDER BY s.start_quantity",
    )?;
    let mut available: BTreeMap<_, _> = available
        .into_iter()
        .map(|part| (part.available.id, part))
        .collect();
    for line in lines {
        if !seen.insert(line.allocation) {
            return Err(Error::Validation(
                "Return each allocation once per note".into(),
            ));
        }
        let mut allocation = available.remove(&line.allocation).ok_or_else(|| {
            Error::Validation("Select an allocation from the original invoice".into())
        })?;
        if line.quantity <= 0 || line.quantity > allocation.available.remaining {
            return Err(Error::Validation(format!(
                "Return quantity exceeds the remaining invoiced quantity for {}",
                allocation.available.item
            )));
        }
        check_quantity_precision(conn, company, allocation.stock.item_id, line.quantity)?;
        let rows = earlier.query_map(params![line.allocation, allocation.offset], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        let spans = stock_notes::reserve_spans(
            allocation.stock.quantity,
            line.quantity,
            allocation.stock.quantity - allocation.available.remaining,
            rows,
        )?
        .into_iter()
        .map(|span| (span.start + allocation.offset)..(span.end + allocation.offset))
        .collect();
        let share = InvoiceQuantityShare::new(allocation.line_quantity, spans);
        let net_amount = i64::try_from(share.apportion(i128::from(allocation.line_net)))
            .map_err(|_| Error::Validation("Return amount is too large".into()))?;
        allocation.stock.quantity = line.quantity;
        planned.push(ReturnPlan {
            allocation,
            share,
            net_amount,
        });
    }
    Ok(planned)
}

pub(crate) fn cost_share(
    conn: &Connection,
    id: i64,
    invoice_line: i64,
    line_quantity: i64,
    returned: i64,
    delivery_quantity: Option<i64>,
) -> Result<InvoiceQuantityShare> {
    let mut stmt = conn.prepare_cached(
        "SELECT start_quantity, quantity FROM invoice_return_spans WHERE return_line_id = ?1 ORDER BY start_quantity",
    )?;
    let share = stock_notes::read_quantity_share(&mut stmt, id, line_quantity, returned)?;
    match delivery_quantity {
        Some(quantity) => {
            stock_notes::invoice_quantity_share(conn, invoice_line, quantity, line_quantity)?
                .project(&share)
        }
        None => Ok(share),
    }
}

#[derive(Debug, Clone)]
pub struct InvoiceReturnLine {
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub quantity: i64,
    pub net_amount: i64,
}

impl Store {
    pub fn invoice_return_allocations(
        &self,
        company: i64,
        invoice: i64,
    ) -> Result<Vec<ReturnAllocation>> {
        Ok(allocations(&self.conn, company, invoice)?
            .into_iter()
            .map(|part| part.available)
            .filter(|part| part.remaining > 0)
            .collect())
    }

    pub fn invoice_return_subtotal(
        &self,
        company: i64,
        invoice: i64,
        lines: &[ReturnLineDraft],
    ) -> Result<i128> {
        Ok(plan(&self.conn, company, invoice, lines)?
            .into_iter()
            .map(|row| i128::from(row.net_amount))
            .sum())
    }

    pub fn invoice_return_lines(&self, company: i64, note: i64) -> Result<Vec<InvoiceReturnLine>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT i.name, i.unit, loc.name, b.name, ret.quantity, ret.net_amount
             FROM invoice_stock_return_history ret
             JOIN stock_items i ON i.id = ret.item_id JOIN stock_locations loc ON loc.id = ret.location_id
             LEFT JOIN stock_batches b ON b.id = ret.batch_id
             WHERE ret.company_id = ?1 AND ret.note_id = ?2 ORDER BY ret.id",
        )?;
        let rows = stmt
            .query_map(params![company, note], |row| {
                Ok(InvoiceReturnLine {
                    item: row.get(0)?,
                    unit: row.get(1)?,
                    location: row.get(2)?,
                    batch: row.get(3)?,
                    quantity: row.get(4)?,
                    net_amount: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn post_invoice_stock_return(
        &mut self,
        company: i64,
        invoice: i64,
        date: &str,
        reference: &str,
        reason: &str,
        lines: &[ReturnLineDraft],
        taxes: &[CreditLineDraft],
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Return reference")?;
        let reason = required(reason, "Return reason")?;
        if taxes.iter().any(|line| !line.is_tax) {
            return Err(Error::Validation(
                "Return trade credits come from the selected invoice quantities".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let kind: String = tx.query_row(
            "SELECT kind FROM invoices WHERE company_id = ?1 AND id = ?2",
            params![company, invoice],
            |row| row.get(0),
        )?;
        let kind = InvoiceKind::parse(&kind)?;
        let planned = plan(&tx, company, invoice, lines)?;
        let mut trade = BTreeMap::<i64, i128>::new();
        for row in &planned {
            *trade.entry(row.allocation.ledger).or_default() += i128::from(row.net_amount);
        }
        let mut credits = taxes.to_vec();
        for (ledger_id, amount) in trade {
            if amount > 0 {
                credits.push(CreditLineDraft {
                    ledger_id,
                    is_tax: false,
                    amount: i64::try_from(amount)
                        .map_err(|_| Error::Validation("Return amount is too large".into()))?,
                });
            }
        }
        let note = crate::invoice_credits::insert_invoice_credit(
            &tx, company, invoice, date, reference, reason, &credits, true,
        )?;
        let movement_reason = format!("Invoice return {reference}: {reason}");
        for row in planned {
            if kind == InvoiceKind::Purchase {
                crate::stock_returns::require_available(&tx, company, date, &row.allocation.stock)?;
            }
            let adjustment = stock_notes::insert_movement(
                &tx,
                company,
                date,
                &row.allocation.stock,
                -stock_notes::stock_direction(kind),
                &movement_reason,
            )?;
            tx.execute(
                "INSERT INTO invoice_return_lines(note_id, allocation_id, quantity, net_amount, adjustment_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![note, row.allocation.available.id, row.allocation.stock.quantity, row.net_amount, adjustment],
            )?;
            let id = tx.last_insert_rowid();
            for span in row.share.spans() {
                tx.execute(
                    "INSERT INTO invoice_return_spans VALUES (?1, ?2, ?3)",
                    params![id, span.start, span.end - span.start],
                )?;
            }
        }
        tx.commit()?;
        Ok(note)
    }
}

pub(crate) fn reverse_movements(
    conn: &Connection,
    company: i64,
    note: i64,
    date: NaiveDate,
    reason: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT ret.id, ret.kind, ret.item_id, ret.location_id, ret.batch_id, ret.quantity,
                COALESCE(n.order_id, link.order_id), i.party_ledger_id
         FROM invoice_stock_return_history ret JOIN invoices i ON i.id = ret.invoice_id
         LEFT JOIN stock_note_lines src ON src.id = ret.delivery_line
         LEFT JOIN stock_notes n ON n.id = src.note_id
         LEFT JOIN invoice_order_links link ON link.invoice_id = i.id AND ret.delivery_line IS NULL
         WHERE ret.company_id = ?1 AND ret.note_id = ?2 ORDER BY ret.id",
    )?;
    let rows = stmt
        .query_map(params![company, note], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                StockNoteLineDraft {
                    item_id: row.get(2)?,
                    location_id: row.get(3)?,
                    batch_id: row.get(4)?,
                    quantity: row.get(5)?,
                    unit_price: 0,
                },
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let Some((_, kind, _, _, party)) = rows.first() else {
        return Ok(());
    };
    let kind = InvoiceKind::parse(kind)?;
    let mut quantities = BTreeMap::<_, Vec<_>>::new();
    for (_, _, stock, order, _) in &rows {
        if let Some(order) = order {
            quantities
                .entry(*order)
                .or_default()
                .push((stock.item_id, stock.quantity));
        }
    }
    for (order, items) in quantities {
        crate::orders::check_fulfilment(
            conn,
            company,
            order,
            kind,
            *party,
            date,
            crate::orders::FulfilmentChange::ReverseReturn,
            items,
        )?;
    }
    for (id, _, stock, _, _) in rows {
        if kind == InvoiceKind::Sale {
            crate::stock_returns::require_available(conn, company, date, &stock)?;
        }
        let adjustment = stock_notes::insert_movement(
            conn,
            company,
            date,
            &stock,
            stock_notes::stock_direction(kind),
            reason,
        )?;
        conn.execute(
            "UPDATE invoice_return_lines SET reversal_adjustment_id = ?1 WHERE id = ?2",
            params![adjustment, id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ReturnLineDraft;
    use crate::actual_cost::ActualCostMethod;
    use crate::inventory::{
        InvoiceAllocation, InvoiceDiscount, InvoiceDraft, InvoiceKind, InvoiceLine,
    };
    use crate::invoice_credits::CreditLineDraft;
    use crate::orders::{OrderDraft, OrderLine, OrderStatus};
    use crate::stock_notes::{StockNoteDraft, StockNoteLineDraft};
    use crate::{Category, Store};

    struct Fixture {
        store: Store,
        company: i64,
        supplier: i64,
        item: i64,
        main: i64,
    }

    impl Fixture {
        fn new() -> Self {
            let store = Store::memory().unwrap();
            let company = store.create_company("Demo").unwrap();
            let supplier = store
                .create_ledger(company, "Supplier", Category::Liability)
                .unwrap();
            let item = store.create_stock_item(company, "Part", "Each").unwrap();
            let main = store.stock_locations(company).unwrap()[0].id;
            Self {
                store,
                company,
                supplier,
                item,
                main,
            }
        }

        fn receipt(&mut self, quantities: &[(i64, i64)]) -> i64 {
            self.store
                .post_stock_note(
                    self.company,
                    &StockNoteDraft {
                        kind: InvoiceKind::Purchase,
                        date: "2026-02-01".into(),
                        reference: "REC-01".into(),
                        party_ledger_id: self.supplier,
                        order_id: None,
                        narration: String::new(),
                        lines: quantities
                            .iter()
                            .map(|&(location_id, quantity)| StockNoteLineDraft {
                                item_id: self.item,
                                location_id,
                                batch_id: None,
                                quantity,
                                unit_price: 500,
                            })
                            .collect(),
                    },
                )
                .unwrap()
        }

        fn draft(&self, kind: InvoiceKind, party: i64, trade: i64, price: i64) -> InvoiceDraft {
            InvoiceDraft {
                kind,
                date: "2026-02-02".into(),
                due_date: None,
                credit_days: None,
                reference: "INV-01".into(),
                narration: String::new(),
                party_ledger_id: party,
                trade_ledger_id: trade,
                tax_ledger_id: None,
                tax_amount: 0,
                additional_taxes: vec![],
                tax_calculation: None,
                adjustments: vec![],
                lines: vec![InvoiceLine {
                    item_id: self.item,
                    location_id: self.main,
                    batch_id: None,
                    trade_ledger_id: None,
                    extra_allocations: vec![],
                    quantity: 3000,
                    unit_price: price,
                    discount: InvoiceDiscount::None,
                }],
            }
        }
    }

    #[test]
    fn allocated_sales_return_credits_discounted_price_tax_and_original_cost() {
        let mut f = Fixture::new();
        let company = f.company;
        let customer = f
            .store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let sales = f
            .store
            .create_ledger(company, "Trade sales", Category::Income)
            .unwrap();
        let tax = f
            .store
            .create_ledger(company, "Sales tax", Category::Liability)
            .unwrap();
        let overflow = f.store.create_stock_location(company, "Overflow").unwrap();
        f.receipt(&[(f.main, 2000), (overflow, 1000)]);
        let order = f
            .store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Sale,
                    party_ledger_id: customer,
                    reference: "SO-01".into(),
                    date: "2026-02-01".into(),
                    expected_on: None,
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: f.item,
                        quantity: 3000,
                        unit_price: 1000,
                    }],
                },
            )
            .unwrap();
        let mut draft = f.draft(InvoiceKind::Sale, customer, sales, 1000);
        draft.tax_ledger_id = Some(tax);
        draft.tax_amount = 300;
        draft.lines[0].discount = InvoiceDiscount::Fixed(100);
        draft.lines[0].extra_allocations.push(InvoiceAllocation {
            location_id: overflow,
            batch_id: None,
            quantity: 1000,
        });
        let invoice = f.store.invoice_order(company, order, &draft).unwrap();
        let allocation = f
            .store
            .invoice_return_allocations(company, invoice)
            .unwrap()
            .into_iter()
            .find(|row| row.location == "Overflow")
            .unwrap()
            .id;
        let lines = [ReturnLineDraft {
            allocation,
            quantity: 1000,
        }];
        assert_eq!(
            f.store
                .invoice_return_subtotal(company, invoice, &lines)
                .unwrap(),
            967
        );
        let note = f
            .store
            .post_invoice_stock_return(
                company,
                invoice,
                "2026-02-02",
                "RET-01",
                "Returned part",
                &lines,
                &[CreditLineDraft {
                    ledger_id: tax,
                    is_tax: true,
                    amount: 100,
                }],
            )
            .unwrap();
        assert!(f
            .store
            .post_invoice_stock_return(
                company,
                invoice,
                "2026-02-03",
                "RET-02",
                "Duplicate units",
                &lines,
                &[]
            )
            .is_err());
        assert_eq!(f.store.invoices(company).unwrap()[0].outstanding, 2133);
        assert_eq!(
            f.store
                .invoice_tax_register(company, None, None, None, None)
                .unwrap()
                .sales,
            200
        );
        let order_line = &f.store.order_detail(company, order).unwrap().lines[0];
        assert_eq!((order_line.fulfilled, order_line.billed), (2000, 2000));
        assert_eq!(
            f.store.invoice_return_lines(company, note).unwrap()[0].location,
            "Overflow"
        );
        for method in ActualCostMethod::ALL {
            let holding = f
                .store
                .stock_valuation_at_actual_cost(company, "2026-02-02", method)
                .unwrap();
            let margin = f
                .store
                .sales_at_actual_cost(company, None, Some("2026-02-02"), method)
                .unwrap();
            assert_eq!((holding.known_value, holding.unavailable), (500, 0));
            assert_eq!(
                (
                    margin.net_sales,
                    margin.cost_of_sales,
                    margin.unavailable_lines
                ),
                (1933, 1000, 0)
            );
        }
        f.store
            .close_order(company, order, "Close remaining quantity")
            .unwrap();
        f.store
            .reverse_invoice_credit(company, note, "2026-02-03", "REV-01", "Reaccepted part")
            .unwrap();
        assert_eq!(f.store.invoices(company).unwrap()[0].outstanding, 3200);
        assert_eq!(
            f.store
                .invoice_tax_register(company, None, None, None, None)
                .unwrap()
                .sales,
            300
        );
        assert_eq!(
            f.store.order_detail(company, order).unwrap().lines[0].fulfilled,
            3000
        );
        assert_eq!(
            f.store.order_detail(company, order).unwrap().summary.status,
            OrderStatus::Closed
        );
        let margin = f
            .store
            .sales_at_actual_cost(company, None, Some("2026-02-03"), ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(
            (
                margin.net_sales,
                margin.cost_of_sales,
                margin.unavailable_lines
            ),
            (2900, 1500, 0)
        );
    }

    #[test]
    fn supplier_return_updates_receipt_without_making_returned_units_billable_again() {
        let mut f = Fixture::new();
        let company = f.company;
        let purchases = f
            .store
            .create_ledger(company, "Trade purchase", Category::Expense)
            .unwrap();
        let stock_note = f.receipt(&[(f.main, 5000)]);
        let draft = f.draft(InvoiceKind::Purchase, f.supplier, purchases, 600);
        let invoice = f
            .store
            .invoice_stock_note(company, stock_note, &draft)
            .unwrap();
        let allocation = f
            .store
            .invoice_return_allocations(company, invoice)
            .unwrap()[0]
            .id;
        let note = f
            .store
            .post_invoice_stock_return(
                company,
                invoice,
                "2026-02-03",
                "RET-01",
                "Defective part",
                &[ReturnLineDraft {
                    allocation,
                    quantity: 1000,
                }],
                &[],
            )
            .unwrap();
        let at_return = f
            .store
            .receipt_reconciliation(company, Some("2026-02-03"))
            .unwrap();
        let row = &at_return[0];
        assert_eq!(
            (
                row.invoiced_quantity,
                row.returned_quantity,
                row.unbilled_quantity()
            ),
            (2000, 1000, 2000)
        );
        assert_eq!(
            (row.invoice_net, row.invoiced_receipt_cost, row.variance()),
            (1200, Some(1000), Some(200))
        );
        assert_eq!(
            f.store.stock_note_lines(company, stock_note).unwrap()[0].unbilled(),
            2000
        );
        let holding = f
            .store
            .stock_valuation_at_actual_cost(company, "2026-02-03", ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!((holding.known_value, holding.unavailable), (2000, 0));
        f.store
            .reverse_invoice_credit(
                company,
                note,
                "2026-02-04",
                "REV-01",
                "Accepted original goods",
            )
            .unwrap();
        let current = f.store.receipt_reconciliation(company, None).unwrap();
        assert_eq!(
            (current[0].invoiced_quantity, current[0].returned_quantity),
            (3000, 0)
        );
        let holding = f
            .store
            .stock_valuation_at_actual_cost(company, "2026-02-04", ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!((holding.known_value, holding.unavailable), (2500, 0));
    }

    #[test]
    fn return_cost_uses_its_original_share_of_a_partly_billed_delivery() {
        let mut f = Fixture::new();
        let company = f.company;
        let customer = f
            .store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let sales = f
            .store
            .create_ledger(company, "Trade sales", Category::Income)
            .unwrap();
        f.receipt(&[(f.main, 5000)]);
        let adjustment = f.store.stock_receipts(company, None).unwrap()[0].id;
        f.store
            .set_stock_receipt_cost(company, adjustment, 2501, "Acquisition amount")
            .unwrap();
        let delivery = f
            .store
            .post_stock_note(
                company,
                &StockNoteDraft {
                    kind: InvoiceKind::Sale,
                    date: "2026-02-02".into(),
                    reference: "DEL-01".into(),
                    party_ledger_id: customer,
                    order_id: None,
                    narration: String::new(),
                    lines: vec![StockNoteLineDraft {
                        item_id: f.item,
                        location_id: f.main,
                        batch_id: None,
                        quantity: 5000,
                        unit_price: 1000,
                    }],
                },
            )
            .unwrap();
        let mut draft = f.draft(InvoiceKind::Sale, customer, sales, 1000);
        draft.lines[0].quantity = 2000;
        f.store
            .invoice_stock_note(company, delivery, &draft)
            .unwrap();
        draft.reference = "INV-02".into();
        draft.lines[0].quantity = 3000;
        let invoice = f
            .store
            .invoice_stock_note(company, delivery, &draft)
            .unwrap();
        let allocation = f
            .store
            .invoice_return_allocations(company, invoice)
            .unwrap()[0]
            .id;
        f.store
            .post_invoice_stock_return(
                company,
                invoice,
                "2026-02-03",
                "RET-01",
                "Returned part",
                &[ReturnLineDraft {
                    allocation,
                    quantity: 1000,
                }],
                &[],
            )
            .unwrap();
        for method in ActualCostMethod::ALL {
            let holding = f
                .store
                .stock_valuation_at_actual_cost(company, "2026-02-03", method)
                .unwrap();
            let margin = f
                .store
                .sales_at_actual_cost(company, None, Some("2026-02-03"), method)
                .unwrap();
            assert_eq!((holding.known_value, holding.unavailable), (501, 0));
            assert_eq!(
                (
                    margin.net_sales,
                    margin.cost_of_sales,
                    margin.unavailable_lines
                ),
                (4000, 2000, 0)
            );
        }
    }
}
