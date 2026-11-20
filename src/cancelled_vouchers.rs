//! Non-posting voucher history.
use crate::{parse_date, Error, Result, Store, VoucherKind};
use rusqlite::OptionalExtension;

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cancelled_vouchers(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            date TEXT NOT NULL,
            voucher_type TEXT NOT NULL,
            reference TEXT NOT NULL,
            party TEXT NOT NULL,
            narration TEXT NOT NULL,
            type_key TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            has_source_id INTEGER NOT NULL CHECK(has_source_id IN (0, 1))
        );
        CREATE INDEX IF NOT EXISTS cancelled_vouchers_company_date ON cancelled_vouchers(company_id, date, id);
        CREATE UNIQUE INDEX IF NOT EXISTS cancelled_vouchers_without_source_id
        ON cancelled_vouchers(company_id, date, type_key, reference_key, party, narration)
        WHERE has_source_id = 0;",
    )?;
    Ok(())
}

pub(crate) fn migrate_optional(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS optional_vouchers(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            date TEXT NOT NULL,
            voucher_type TEXT NOT NULL,
            reference TEXT NOT NULL,
            party TEXT NOT NULL,
            narration TEXT NOT NULL,
            payload TEXT NOT NULL,
            has_source_id INTEGER NOT NULL CHECK(has_source_id IN (0, 1))
        );
        CREATE INDEX IF NOT EXISTS optional_vouchers_company_date ON optional_vouchers(company_id, date, id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct InactiveVoucher {
    pub id: i64,
    pub date: String,
    pub voucher_type: String,
    pub reference: String,
    pub party: String,
    pub narration: String,
}

pub type CancelledVoucher = InactiveVoucher;

impl Store {
    pub fn cancelled_vouchers(
        &self,
        company_id: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<VoucherKind>,
        search: &str,
    ) -> Result<Vec<CancelledVoucher>> {
        self.inactive_vouchers("cancelled_vouchers", company_id, start, end, kind, search)
    }

    pub fn optional_vouchers(
        &self,
        company_id: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<VoucherKind>,
        search: &str,
    ) -> Result<Vec<InactiveVoucher>> {
        self.inactive_vouchers("optional_vouchers", company_id, start, end, kind, search)
    }

    /// Return the retained XML for review or transfer on the local machine.
    pub fn optional_voucher_xml(&self, company_id: i64, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT payload FROM optional_vouchers WHERE company_id = ?1 AND id = ?2",
                rusqlite::params![company_id, id],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn inactive_vouchers(
        &self,
        table: &str,
        company_id: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<VoucherKind>,
        search: &str,
    ) -> Result<Vec<InactiveVoucher>> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(a), Some(b)) if a > b) {
            return Err(Error::Validation(
                "The start date must not be after the end date".into(),
            ));
        }
        let search = search.trim().to_lowercase();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, date, voucher_type, reference, party, narration FROM {table}
             WHERE company_id = ?1"
        ))?;
        let rows = stmt.query_map([company_id], |row| {
            Ok(InactiveVoucher {
                id: row.get(0)?,
                date: row.get(1)?,
                voucher_type: row.get(2)?,
                reference: row.get(3)?,
                party: row.get(4)?,
                narration: row.get(5)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let voucher = row?;
            let date = parse_date(&voucher.date)?;
            if start.is_some_and(|from| date < from)
                || end.is_some_and(|through| date > through)
                || kind.is_some_and(|kind| {
                    crate::exchange::parse_kind(&voucher.voucher_type) != Some(kind)
                })
            {
                continue;
            }
            if search.is_empty()
                || voucher.id.to_string().contains(&search)
                || [
                    &voucher.reference,
                    &voucher.voucher_type,
                    &voucher.party,
                    &voucher.narration,
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(&search))
            {
                entries.push((date, voucher));
            }
        }
        entries.sort_by_key(|(date, voucher)| std::cmp::Reverse((*date, voucher.id)));
        Ok(entries.into_iter().map(|(_, voucher)| voucher).collect())
    }
}
