//! Saved sales and purchase quotations and their conversion to orders.

use crate::inventory::InvoiceKind;
use crate::orders::{insert_order, validate_order_draft, OrderDraft, OrderLine};
use crate::{parse_date, required, Error, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE quotations(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            kind TEXT NOT NULL CHECK(kind IN ('Sale', 'Purchase')),
            party_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            date TEXT NOT NULL,
            expected_on TEXT,
            valid_through TEXT,
            notes TEXT NOT NULL,
            order_id INTEGER UNIQUE REFERENCES orders(id),
            converted_at TEXT,
            closed_at TEXT,
            close_reason TEXT,
            CHECK((closed_at IS NULL) = (close_reason IS NULL)),
            CHECK((order_id IS NULL) = (converted_at IS NULL)),
            CHECK(order_id IS NULL OR closed_at IS NULL),
            UNIQUE(company_id, kind, party_ledger_id, reference_key)
        );
        CREATE INDEX quotations_company ON quotations(company_id, id);
        CREATE TABLE quotation_lines(
            id INTEGER PRIMARY KEY,
            quotation_id INTEGER NOT NULL REFERENCES quotations(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            unit_price INTEGER NOT NULL CHECK(unit_price >= 0),
            UNIQUE(quotation_id, item_id)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Quotation {
    pub id: i64,
    pub kind: InvoiceKind,
    pub party_ledger_id: i64,
    pub party: String,
    pub reference: String,
    pub date: String,
    pub expected_on: Option<String>,
    pub valid_through: Option<String>,
    pub notes: String,
    pub order_id: Option<i64>,
    pub converted_at: Option<String>,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
}

impl Quotation {
    pub fn status(&self) -> &'static str {
        if self.order_id.is_some() {
            "Converted"
        } else if self.closed_at.is_some() {
            "Closed"
        } else {
            "Open"
        }
    }
}

fn quotations(conn: &Connection, company: i64, id: Option<i64>) -> Result<Vec<Quotation>> {
    let mut stmt = conn.prepare(
        "SELECT q.id, q.kind, q.party_ledger_id, l.name, q.reference, q.date,
                q.expected_on, q.valid_through, q.notes, q.order_id,
                q.converted_at, q.closed_at, q.close_reason
         FROM quotations q JOIN ledgers l ON l.id = q.party_ledger_id AND l.company_id = q.company_id
         WHERE q.company_id = ?1 AND (?2 IS NULL OR q.id = ?2) ORDER BY q.id DESC",
    )?;
    let rows = stmt.query_and_then(params![company, id], |row| {
        Ok(Quotation {
            id: row.get(0)?,
            kind: InvoiceKind::parse(&row.get::<_, String>(1)?)?,
            party_ledger_id: row.get(2)?,
            party: row.get(3)?,
            reference: row.get(4)?,
            date: row.get(5)?,
            expected_on: row.get(6)?,
            valid_through: row.get(7)?,
            notes: row.get(8)?,
            order_id: row.get(9)?,
            converted_at: row.get(10)?,
            closed_at: row.get(11)?,
            close_reason: row.get(12)?,
        })
    })?;
    rows.collect()
}

fn quotation_lines(conn: &Connection, company: i64, quote: i64) -> Result<Vec<OrderLine>> {
    let mut stmt = conn.prepare(
        "SELECT l.item_id, l.quantity, l.unit_price
         FROM quotation_lines l JOIN quotations q ON q.id = l.quotation_id
         WHERE q.company_id = ?1 AND q.id = ?2 ORDER BY l.id",
    )?;
    let rows = stmt
        .query_map(params![company, quote], |row| {
            Ok(OrderLine {
                item_id: row.get(0)?,
                quantity: row.get(1)?,
                unit_price: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

impl Store {
    pub fn create_quotation(
        &mut self,
        company: i64,
        draft: &OrderDraft,
        valid_through: Option<&str>,
    ) -> Result<i64> {
        let reference = required(&draft.reference, "Quotation reference")?;
        let valid_through = valid_through.map(parse_date).transpose()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (date, expected) = validate_order_draft(&tx, company, draft)?;
        if valid_through.is_some_and(|expiry| expiry < date) {
            return Err(Error::Validation(
                "The validity date must be on or after the quotation date".into(),
            ));
        }

        let changed = tx.execute(
            "INSERT INTO quotations(company_id, kind, party_ledger_id, reference, reference_key,
                                     date, expected_on, valid_through, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(company_id, kind, party_ledger_id, reference_key) DO NOTHING",
            params![
                company,
                draft.kind.as_str(),
                draft.party_ledger_id,
                reference,
                reference.to_lowercase(),
                date.to_string(),
                expected.map(|date| date.to_string()),
                valid_through.map(|date| date.to_string()),
                draft.notes.trim(),
            ],
        )?;
        if changed != 1 {
            return Err(Error::Validation(
                "A quotation with this type, party and reference already exists".into(),
            ));
        }
        let id = tx.last_insert_rowid();
        for line in &draft.lines {
            tx.execute(
                "INSERT INTO quotation_lines(quotation_id, item_id, quantity, unit_price)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, line.item_id, line.quantity, line.unit_price],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn quotations(&self, company: i64) -> Result<Vec<Quotation>> {
        quotations(&self.conn, company, None)
    }

    pub fn quotation_lines(&self, company: i64, quote: i64) -> Result<Vec<OrderLine>> {
        quotation_lines(&self.conn, company, quote)
    }

    pub fn order_quotation(&self, company: i64, order: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT reference FROM quotations WHERE company_id = ?1 AND order_id = ?2",
                params![company, order],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn close_quotation(&mut self, company: i64, quote: i64, reason: &str) -> Result<()> {
        let reason = required(reason, "Close reason")?;
        let changed = self.conn.execute(
            "UPDATE quotations SET closed_at = CURRENT_TIMESTAMP, close_reason = ?1
             WHERE company_id = ?2 AND id = ?3 AND order_id IS NULL AND closed_at IS NULL",
            params![reason, company, quote],
        )?;
        if changed != 1 {
            return Err(Error::Validation("Select an open quotation".into()));
        }
        Ok(())
    }

    pub fn convert_quotation(
        &mut self,
        company: i64,
        quotation: i64,
        order_reference: &str,
        order_date: &str,
        expected_on: Option<&str>,
    ) -> Result<i64> {
        let date = parse_date(order_date)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let quote = quotations(&tx, company, Some(quotation))?
            .into_iter()
            .next()
            .filter(|quote| quote.closed_at.is_none() && quote.order_id.is_none())
            .ok_or_else(|| Error::Validation("Select an open quotation".into()))?;
        if date < parse_date(&quote.date)? {
            return Err(Error::Validation(
                "The order date must be on or after the quotation date".into(),
            ));
        }
        let valid_through = quote.valid_through.as_deref().map(parse_date).transpose()?;
        if valid_through.is_some_and(|expiry| date > expiry) {
            return Err(Error::Validation(
                "The quotation has expired on the order date".into(),
            ));
        }

        let draft = OrderDraft {
            kind: quote.kind,
            party_ledger_id: quote.party_ledger_id,
            reference: order_reference.to_owned(),
            date: date.to_string(),
            expected_on: expected_on.map(str::to_owned),
            notes: quote.notes,
            lines: quotation_lines(&tx, company, quotation)?,
        };
        let order = insert_order(&tx, company, &draft)?;
        tx.execute(
            "UPDATE quotations SET order_id = ?1, converted_at = CURRENT_TIMESTAMP
             WHERE company_id = ?2 AND id = ?3",
            params![order, company, quotation],
        )?;
        tx.commit()?;
        Ok(order)
    }
}
