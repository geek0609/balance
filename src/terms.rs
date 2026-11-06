//! Optional party defaults; credit limits are informational only.
use crate::{Error, Result, Store};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerTerms {
    pub credit_days: Option<u32>,
    pub credit_limit: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct TermsRevision {
    pub changed_at: String,
    pub terms: LedgerTerms,
}

impl Store {
    pub fn ledger_terms(&self, company: i64, ledger: i64) -> Result<LedgerTerms> {
        let eligible: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM ledgers WHERE id = ?1 AND company_id = ?2 AND category IN ('Asset', 'Liability'))", params![ledger, company], |r| r.get(0))?;
        if !eligible {
            return Err(Error::Validation(
                "Choose an asset or liability ledger in the selected company".into(),
            ));
        }
        Ok(self.conn.query_row("SELECT credit_days, credit_limit FROM ledger_terms WHERE company_id = ?1 AND ledger_id = ?2", params![company, ledger], |r| Ok(LedgerTerms { credit_days: r.get(0)?, credit_limit: r.get(1)? })).optional()?.unwrap_or_default())
    }

    pub fn save_ledger_terms(
        &mut self,
        company: i64,
        ledger: i64,
        terms: &LedgerTerms,
    ) -> Result<()> {
        if terms.credit_limit.is_some_and(|limit| limit < 0) {
            return Err(Error::Validation("Credit limit cannot be negative".into()));
        }
        if self.ledger_terms(company, ledger)? == *terms {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO ledger_term_revisions(company_id, ledger_id, credit_days, credit_limit) SELECT company_id, ledger_id, credit_days, credit_limit FROM ledger_terms WHERE company_id = ?1 AND ledger_id = ?2", params![company, ledger])?;
        tx.execute("INSERT INTO ledger_terms(company_id, ledger_id, credit_days, credit_limit) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(ledger_id) DO UPDATE SET credit_days=excluded.credit_days, credit_limit=excluded.credit_limit", params![company, ledger, terms.credit_days, terms.credit_limit])?;
        tx.commit()?;
        Ok(())
    }

    pub fn ledger_terms_history(&self, company: i64, ledger: i64) -> Result<Vec<TermsRevision>> {
        self.ledger_terms(company, ledger)?;
        let mut stmt = self.conn.prepare("SELECT changed_at, credit_days, credit_limit FROM ledger_term_revisions WHERE company_id = ?1 AND ledger_id = ?2 ORDER BY id DESC")?;
        let rows = stmt
            .query_map(params![company, ledger], |r| {
                Ok(TermsRevision {
                    changed_at: r.get(0)?,
                    terms: LedgerTerms {
                        credit_days: r.get(1)?,
                        credit_limit: r.get(2)?,
                    },
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
