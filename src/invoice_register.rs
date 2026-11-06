//! Filtered sales and purchase invoice register.
use crate::{
    inventory::{InvoiceKind, InvoiceSummary},
    parse_date, Error, Result, Store,
};
use rusqlite::params;

impl Store {
    /// Settlement figures reflect all saved settlements and reversals, including future dates.
    pub fn invoice_register(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        kind: Option<InvoiceKind>,
        party: Option<i64>,
        search: &str,
    ) -> Result<Vec<InvoiceSummary>> {
        if !self
            .companies()?
            .iter()
            .any(|company_row| company_row.id == company)
        {
            return Err(Error::Validation("Company not found".into()));
        }
        let from = start.map(parse_date).transpose()?;
        let to = end.map(parse_date).transpose()?;
        if matches!((from, to), (Some(a), Some(b)) if a > b) {
            return Err(Error::Validation(
                "The start date must not be after the end date".into(),
            ));
        }
        if let Some(party) = party {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM ledgers WHERE company_id = ?1 AND id = ?2)",
                params![company, party],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "Party ledger not found in the selected company".into(),
                ));
            }
        }
        let search = search.trim().to_lowercase();
        let mut result = Vec::new();
        for mut invoice in self.invoices(company)? {
            let date = parse_date(&invoice.date)?;
            if from.is_some_and(|value| date < value)
                || to.is_some_and(|value| date > value)
                || kind.is_some_and(|value| value != invoice.kind)
                || party.is_some_and(|value| value != invoice.party_ledger_id)
                || (!search.is_empty()
                    && !invoice.reference.to_lowercase().contains(&search)
                    && !invoice.party.to_lowercase().contains(&search)
                    && !invoice.id.to_string().contains(&search))
            {
                continue;
            }
            invoice.date = date.format("%Y-%m-%d").to_string();
            result.push(invoice);
        }
        result.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
        Ok(result)
    }
}
