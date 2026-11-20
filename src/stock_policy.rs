//! Company settings for nonnegative dated warehouse stock balances.
use crate::inventory::{format_quantity, stock_bucket_changes, StockChanges};
use crate::{parse_date, required, Error, Result, Store};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::BTreeMap;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE negative_stock_policy_changes(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            blocked_from TEXT,
            reason TEXT NOT NULL,
            changed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX negative_stock_policy_changes_company ON negative_stock_policy_changes(company_id, id);
        CREATE VIEW warehouse_stock_movements AS
            SELECT company_id, item_id, location_id, batch_id, date, quantity, 0 AS location_only
            FROM stock_adjustments
            UNION ALL
            SELECT i.company_id, l.item_id, a.location_id, a.batch_id, v.date,
                   CASE i.kind WHEN 'Purchase' THEN a.quantity ELSE -a.quantity END, 0
            FROM invoice_line_allocations a JOIN physical_invoice_lines l ON l.id = a.line_id
            JOIN invoices i ON i.id = l.invoice_id JOIN vouchers v ON v.id = i.voucher_id
            UNION ALL
            SELECT company_id, item_id, location_id, batch_id, date, quantity, 1
            FROM stock_transfer_location_movements;",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct NegativeStockPolicyChange {
    pub blocked_from: Option<NaiveDate>,
    pub reason: String,
    pub changed_at: String,
}

fn blocked_from(conn: &Connection, company: i64) -> Result<Option<NaiveDate>> {
    let date: Option<String> = conn.prepare_cached(
        "SELECT blocked_from FROM negative_stock_policy_changes WHERE company_id = ?1 ORDER BY id DESC LIMIT 1",
    )?.query_row([company], |r| r.get(0)).optional()?.flatten();
    date.as_deref().map(parse_date).transpose()
}

fn check_closing_balances(
    conn: &Connection,
    item: i64,
    location: i64,
    batch: Option<i64>,
    from: NaiveDate,
    changes: StockChanges,
) -> Result<()> {
    if let Some((date, balance)) = changes
        .closing_balances(from)
        .find(|(_, balance)| *balance < 0)
    {
        let (item_name, location_name, batch_name): (String, String, Option<String>) = conn
            .query_row(
                "SELECT s.name, l.name, b.name FROM stock_items s CROSS JOIN stock_locations l
             LEFT JOIN stock_batches b ON b.id = ?3 WHERE s.id = ?1 AND l.id = ?2",
                params![item, location, batch],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        return Err(Error::Validation(format!(
            "{item_name} at {location_name} ({} batch) would have a closing quantity of {} on {date}. Negative stock is blocked.",
            batch_name.as_deref().unwrap_or("no"),
            format_quantity(balance),
        )));
    }
    Ok(())
}

pub(crate) fn check_bucket(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
) -> Result<()> {
    if let Some(from) = blocked_from(conn, company)? {
        let changes = stock_bucket_changes(conn, company, item, location, batch, from)?;
        check_closing_balances(conn, item, location, batch, from, changes)?;
    }
    Ok(())
}

fn check_company_from(conn: &Connection, company: i64, from: NaiveDate) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT item_id, location_id, batch_id, date, quantity FROM warehouse_stock_movements WHERE company_id = ?1",
    )?;
    let mut buckets = BTreeMap::<(i64, i64, Option<i64>), StockChanges>::new();
    let mut rows = stmt.query([company])?;
    while let Some(row) = rows.next()? {
        let date = parse_date(&row.get::<_, String>(3)?)?;
        buckets
            .entry((row.get(0)?, row.get(1)?, row.get(2)?))
            .or_default()
            .add(from, date, row.get(4)?);
    }
    for ((item, location, batch), changes) in buckets {
        check_closing_balances(conn, item, location, batch, from, changes)?;
    }
    Ok(())
}

/// Validate the final warehouse state of a multi-document import.
pub(crate) fn check_import(conn: &Connection, company: i64) -> Result<()> {
    if let Some(from) = blocked_from(conn, company)? {
        check_company_from(conn, company, from)?;
    }
    Ok(())
}

impl Store {
    pub fn negative_stock_blocked_from(&self, company: i64) -> Result<Option<NaiveDate>> {
        blocked_from(&self.conn, company)
    }

    pub fn set_negative_stock_policy(
        &mut self,
        company: i64,
        from: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let from = from.map(parse_date).transpose()?;
        let reason = required(reason, "Reason for changing the stock policy")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if blocked_from(&tx, company)? == from {
            return Err(Error::Validation(
                "The stock policy already uses this setting".into(),
            ));
        }
        if let Some(date) = from {
            check_company_from(&tx, company, date)?;
        }
        tx.execute(
            "INSERT INTO negative_stock_policy_changes(company_id, blocked_from, reason) VALUES (?1, ?2, ?3)",
            params![company, from.map(|date| date.to_string()), reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn negative_stock_policy_history(
        &self,
        company: i64,
    ) -> Result<Vec<NegativeStockPolicyChange>> {
        let mut stmt = self.conn.prepare(
            "SELECT blocked_from, reason, changed_at FROM negative_stock_policy_changes
             WHERE company_id = ?1 ORDER BY id DESC",
        )?;
        let mut rows = stmt.query([company])?;
        let mut history = Vec::new();
        while let Some(row) = rows.next()? {
            let from: Option<String> = row.get(0)?;
            history.push(NegativeStockPolicyChange {
                blocked_from: from.as_deref().map(parse_date).transpose()?,
                reason: row.get(1)?,
                changed_at: row.get(2)?,
            });
        }
        Ok(history)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{InvoiceDiscount, InvoiceDraft, InvoiceKind, InvoiceLine};
    use crate::stock_counts::{CountLine, StockCountDraft};
    use crate::stock_transit::StockDispatchDraft;
    use crate::Category;

    #[test]
    fn dated_policy_rejects_future_shortages_without_moving_old_entries() {
        let store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let purchases = store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let sales = store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let branch = store.create_stock_location(company, "Annex").unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2026-01-01",
                -1000,
                "Early issue",
            )
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2026-01-02",
                6000,
                "Opening receipt",
            )
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP VIEW bill_note_refund_history;
                 DROP TABLE bill_note_refunds;
                 DROP TABLE bill_note_credit_applications;
                 DROP TABLE bill_note_credits;
                 DROP TABLE bill_note_allocations;
                 DROP TABLE bill_split_note_credits;
                 DROP TABLE bill_split_credit_allocations;
                 DROP INDEX credit_notes_with_invoice;
                 DROP INDEX credit_allocations_with_invoice;
                 DROP TABLE bill_split_advances;
                 DROP INDEX advance_allocations_with_invoice;
                 DROP TABLE bill_split_settlements;
                 DROP TABLE invoice_bill_split_revisions;
                 DROP TABLE invoice_bill_splits;
                 DROP INDEX invoice_settlements_with_invoice;
                 ALTER TABLE invoices DROP COLUMN entry_mode;
                 ALTER TABLE invoice_revisions DROP COLUMN entry_mode;
                 ALTER TABLE invoices DROP COLUMN bill_reference;
                 ALTER TABLE invoice_revisions DROP COLUMN bill_reference;
                 ALTER TABLE postings DROP COLUMN is_party;
                 ALTER TABLE revision_postings DROP COLUMN is_party;
                 ALTER TABLE recurring_journal_lines DROP COLUMN is_party;
                 DROP TABLE quotation_revisions;
                 DROP VIEW active_stock_reservation_balances;
                 DROP TABLE stock_reservation_expiries;
                 DROP VIEW warehouse_stock_movements;
             DROP TABLE negative_stock_policy_changes;
             PRAGMA user_version = 68;",
            )
            .unwrap();
        let mut store = Store::from_connection(store.conn).unwrap();
        assert_eq!(store.negative_stock_blocked_from(company).unwrap(), None);
        let mut draft = InvoiceDraft {
            external_reference: Default::default(),
            kind: InvoiceKind::Purchase,
            entry_mode: crate::inventory::InvoiceEntryMode::Invoice,
            date: "2026-02-01".into(),
            reference: "BUY-01".into(),
            bill_reference: None,
            bill_splits: Vec::new(),
            narration: String::new(),
            due_date: None,
            credit_days: None,
            party_ledger_id: supplier,
            trade_ledger_id: purchases,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: Vec::new(),
            tax_calculation: None,
            adjustments: Vec::new(),
            lines: vec![InvoiceLine {
                rate_basis: None,
                item_id: item,
                location_id: location,
                batch_id: None,
                quantity: 5000,
                unit_price: 1000,
                trade_ledger_id: None,
                discount: InvoiceDiscount::None,
                extra_allocations: Vec::new(),
            }],
        };
        let buy = store.create_invoice(company, &draft).unwrap();
        let mut sale = draft.clone();
        sale.kind = InvoiceKind::Sale;
        sale.party_ledger_id = customer;
        sale.trade_ledger_id = sales;
        sale.date = "2026-02-05".into();
        sale.reference = "SELL-01".into();
        sale.lines[0].quantity = 7000;
        store.create_invoice(company, &sale).unwrap();

        assert!(store
            .set_negative_stock_policy(company, Some("2026-01-01"), "Start enforcement")
            .is_err());
        assert!(store
            .negative_stock_policy_history(company)
            .unwrap()
            .is_empty());
        store
            .set_negative_stock_policy(company, Some("2026-01-03"), "Start enforcement")
            .unwrap();
        assert!(store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2026-01-02",
                -6000,
                "Backdated issue"
            )
            .unwrap_err()
            .to_string()
            .contains("2026-01-03"));
        let failure = store
            .transfer_stock(company, item, location, branch, "2026-01-04", 4000, "Move")
            .unwrap_err();
        assert!(failure.to_string().contains("2026-02-05"));
        assert!(store.stock_transfers(company).unwrap().is_empty());
        let shipment = StockDispatchDraft {
            item_id: item,
            batch_id: None,
            from_location_id: location,
            to_location_id: branch,
            date: "2026-01-04".into(),
            quantity: 4000,
            reference: "MOVE-01".into(),
            reason: "Ship to annex".into(),
        };
        assert!(store.dispatch_stock(company, &shipment).is_err());
        assert!(store.stock_dispatches(company, None).unwrap().is_empty());
        draft.lines[0].quantity = 1000;
        assert!(store
            .update_invoice(company, buy, &draft)
            .unwrap_err()
            .to_string()
            .contains("2026-02-05"));
        assert_eq!(
            store.invoice_draft(company, buy).unwrap().lines[0].quantity,
            5000
        );
        assert!(store
            .reverse_invoice(company, buy, "2026-02-06", "CANCEL", "Cancel purchase")
            .is_err());
        sale.date = "2026-02-06".into();
        sale.reference = "SELL-02".into();
        sale.lines[0].quantity = 4000;
        assert!(store.create_invoice(company, &sale).is_err());
        assert_eq!(
            store
                .stock_location_balances(company, location, None)
                .unwrap()[0]
                .quantity,
            3000
        );
        assert_eq!(
            store
                .stock_location_balances(company, location, Some("2026-01-01"))
                .unwrap()[0]
                .quantity,
            -1000
        );
        let count = store
            .preview_stock_count(
                company,
                &StockCountDraft {
                    location_id: location,
                    date: "2026-01-06".into(),
                    reference: "COUNT-01".into(),
                    notes: String::new(),
                    lines: vec![CountLine {
                        item_id: item,
                        batch_id: None,
                        counted_quantity: 1000,
                    }],
                },
            )
            .unwrap();
        assert!(store.post_stock_count(company, &count).is_err());
        assert!(store.stock_counts(company).unwrap().is_empty());

        let lot = store
            .create_stock_batch(company, item, "Lot 1", None, None)
            .unwrap();
        assert!(store
            .adjust_stock_batch(
                company,
                item,
                location,
                Some(lot),
                "2026-02-06",
                -1000,
                "Unreceived batch"
            )
            .is_err());
        store
            .transfer_stock(
                company,
                item,
                location,
                branch,
                "2026-02-06",
                3000,
                "Move balance",
            )
            .unwrap();
        assert_eq!(
            store
                .stock_location_balances(company, location, None)
                .unwrap()[0]
                .quantity,
            0
        );
        store
            .set_negative_stock_policy(company, None, "Allow negative balances")
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                location,
                None,
                "2026-02-06",
                -1000,
                "Legacy behaviour",
            )
            .unwrap();
        let history = store.negative_stock_policy_history(company).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].blocked_from, None);
        assert_eq!(
            history[1].blocked_from,
            Some(parse_date("2026-01-03").unwrap())
        );
    }
}
