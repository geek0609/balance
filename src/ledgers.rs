//! Renaming and regrouping accounts while preserving their classification.
use crate::{required, Error, Result, Store};
use rusqlite::params;

#[derive(Debug, Clone)]
pub struct LedgerRevision {
    pub changed_at: String,
    pub name: String,
    pub group: Option<String>,
}

impl Store {
    pub fn update_ledger(
        &mut self,
        company: i64,
        ledger: i64,
        name: &str,
        group: i64,
    ) -> Result<()> {
        let name = required(name, "Ledger name")?;
        let tx = self.conn.transaction()?;
        let (old_name, old_group, category): (String, Option<i64>, String) = tx
            .query_row(
                "SELECT name, group_id, category FROM ledgers WHERE id = ?1 AND company_id = ?2",
                params![ledger, company],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Ledger not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        let is_control: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM company_controls WHERE company_id = ?1 AND opening_difference_ledger_id = ?2)", params![company, ledger], |r| r.get(0))?;
        if is_control {
            return Err(Error::Validation(
                "The opening difference ledger is maintained automatically".into(),
            ));
        }
        if name.to_lowercase() == "opening balance difference" {
            return Err(Error::Validation("That ledger name is reserved".into()));
        }
        let group_category: String = tx
            .query_row(
                "SELECT category FROM account_groups WHERE id = ?1 AND company_id = ?2",
                params![group, company],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Group not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        if group_category != category {
            return Err(Error::Validation(
                "The new group must have the same account category".into(),
            ));
        }
        if old_name != name || old_group != Some(group) {
            tx.execute("INSERT INTO ledger_revisions(company_id, ledger_id, name, group_id) VALUES (?1, ?2, ?3, ?4)", params![company, ledger, old_name, old_group])?;
            tx.execute(
                "UPDATE ledgers SET name = ?1, group_id = ?2 WHERE company_id = ?3 AND id = ?4",
                params![name, group, company, ledger],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn ledger_revisions(&self, company: i64, ledger: i64) -> Result<Vec<LedgerRevision>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ledgers WHERE company_id = ?1 AND id = ?2)",
            params![company, ledger],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Ledger not found in the selected company".into(),
            ));
        }
        let mut stmt = self.conn.prepare("SELECT r.changed_at, r.name, g.name FROM ledger_revisions r LEFT JOIN account_groups g ON g.id = r.group_id AND g.company_id = r.company_id WHERE r.company_id = ?1 AND r.ledger_id = ?2 ORDER BY r.id DESC")?;
        let rows = stmt
            .query_map(params![company, ledger], |r| {
                Ok(LedgerRevision {
                    changed_at: r.get(0)?,
                    name: r.get(1)?,
                    group: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
