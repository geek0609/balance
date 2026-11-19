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

pub(crate) fn migrate_revisions(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE quotation_revisions(
            original_id INTEGER PRIMARY KEY REFERENCES quotations(id),
            replacement_id INTEGER NOT NULL UNIQUE REFERENCES quotations(id),
            CHECK(original_id != replacement_id)
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
    pub revises: Option<String>,
    pub revised_by: Option<String>,
}

impl Quotation {
    pub fn status(&self) -> &'static str {
        if self.order_id.is_some() {
            "Converted"
        } else if self.revised_by.is_some() {
            "Revised"
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
                q.converted_at, q.closed_at, q.close_reason,
                (SELECT original.reference FROM quotation_revisions r
                 JOIN quotations original ON original.id = r.original_id WHERE r.replacement_id = q.id),
                (SELECT replacement.reference FROM quotation_revisions r
                 JOIN quotations replacement ON replacement.id = r.replacement_id WHERE r.original_id = q.id)
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
            revises: row.get(13)?,
            revised_by: row.get(14)?,
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

pub(crate) fn quotation(conn: &Connection, company: i64, id: i64) -> Result<Quotation> {
    quotations(conn, company, Some(id))?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Validation("Quotation not found in the selected company".into()))
}

fn insert_quotation(
    conn: &Connection,
    company: i64,
    draft: &OrderDraft,
    valid_through: Option<&str>,
) -> Result<i64> {
    let reference = required(&draft.reference, "Quotation reference")?;
    let valid_through = valid_through.map(parse_date).transpose()?;
    let (date, expected) = validate_order_draft(conn, company, draft)?;
    if valid_through.is_some_and(|expiry| expiry < date) {
        return Err(Error::Validation(
            "The validity date must be on or after the quotation date".into(),
        ));
    }

    let changed = conn.execute(
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
    let id = conn.last_insert_rowid();
    for line in &draft.lines {
        conn.execute(
            "INSERT INTO quotation_lines(quotation_id, item_id, quantity, unit_price)
                 VALUES (?1, ?2, ?3, ?4)",
            params![id, line.item_id, line.quantity, line.unit_price],
        )?;
    }
    Ok(id)
}

impl Store {
    pub fn create_quotation(
        &mut self,
        company: i64,
        draft: &OrderDraft,
        valid_through: Option<&str>,
    ) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = insert_quotation(&tx, company, draft, valid_through)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn revise_quotation(
        &mut self,
        company: i64,
        original: i64,
        draft: &OrderDraft,
        valid_through: Option<&str>,
        reason: &str,
    ) -> Result<i64> {
        let reason = required(reason, "Revision reason")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let quote = quotation(&tx, company, original)?;
        if quote.closed_at.is_some() || quote.order_id.is_some() {
            return Err(Error::Validation("Select an open quotation".into()));
        }
        if draft.kind != quote.kind || draft.party_ledger_id != quote.party_ledger_id {
            return Err(Error::Validation(
                "Keep the original party and quotation type".into(),
            ));
        }
        if parse_date(&draft.date)? < parse_date(&quote.date)? {
            return Err(Error::Validation(
                "Revision date cannot precede the original quotation".into(),
            ));
        }
        let replacement = insert_quotation(&tx, company, draft, valid_through)?;
        tx.execute(
            "UPDATE quotations SET closed_at = CURRENT_TIMESTAMP, close_reason = ?1 WHERE id = ?2",
            params![reason, original],
        )?;
        tx.execute(
            "INSERT INTO quotation_revisions(original_id, replacement_id) VALUES (?1, ?2)",
            params![original, replacement],
        )?;
        tx.commit()?;
        Ok(replacement)
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
        quote_id: i64,
        order_reference: &str,
        order_date: &str,
        expected_on: Option<&str>,
    ) -> Result<i64> {
        let date = parse_date(order_date)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let quote = quotation(&tx, company, quote_id)?;
        if quote.closed_at.is_some() || quote.order_id.is_some() {
            return Err(Error::Validation("Select an open quotation".into()));
        }
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
            lines: quotation_lines(&tx, company, quote_id)?,
        };
        let order = insert_order(&tx, company, &draft)?;
        tx.execute(
            "UPDATE quotations SET order_id = ?1, converted_at = CURRENT_TIMESTAMP
             WHERE company_id = ?2 AND id = ?3",
            params![order, company, quote_id],
        )?;
        tx.commit()?;
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_preserves_old_quote_and_converts_only_the_new_terms() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", crate::Category::Asset)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let mut draft = OrderDraft {
            kind: InvoiceKind::Sale,
            party_ledger_id: customer,
            reference: "QT-01".into(),
            date: "2026-01-01".into(),
            expected_on: None,
            notes: String::new(),
            lines: vec![OrderLine {
                item_id: item,
                quantity: 1000,
                unit_price: 2500,
            }],
        };
        let original = store.create_quotation(company, &draft, None).unwrap();
        assert!(store
            .revise_quotation(company, original, &draft, None, "Price change")
            .is_err());
        assert_eq!(
            quotation(&store.conn, company, original).unwrap().status(),
            "Open"
        );
        draft.reference = "QT-01B".into();
        draft.lines[0].unit_price = 3000;
        let replacement = store
            .revise_quotation(company, original, &draft, None, "Price change")
            .unwrap();
        let prior = quotation(&store.conn, company, original).unwrap();
        assert_eq!(prior.status(), "Revised");
        assert_eq!(prior.revised_by.as_deref(), Some("QT-01B"));
        assert_eq!(
            quotation(&store.conn, company, replacement)
                .unwrap()
                .revises
                .as_deref(),
            Some("QT-01")
        );
        assert_eq!(
            store.quotation_lines(company, original).unwrap()[0].unit_price,
            2500
        );
        assert!(store
            .convert_quotation(company, original, "SO-01", "2026-01-02", None)
            .is_err());
        let order = store
            .convert_quotation(company, replacement, "SO-01", "2026-01-02", None)
            .unwrap();
        assert_eq!(
            store.order_detail(company, order).unwrap().lines[0]
                .line
                .unit_price,
            3000
        );
    }
}
