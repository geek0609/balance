//! Optional contact details attached to company ledgers.
use crate::{Error, Result, Store};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerProfile {
    pub mailing_name: String,
    pub address: String,
    pub postal_code: String,
    pub country: String,
    pub region: String,
    pub email: String,
    pub email_cc: String,
    pub phone: String,
    pub mobile: String,
    pub tax_id: String,
    pub tax_registration_type: String,
}
#[derive(Debug, Clone)]
pub struct ProfileRevision {
    pub changed_at: String,
    pub profile: LedgerProfile,
}

fn profile_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<LedgerProfile> {
    Ok(LedgerProfile {
        mailing_name: row.get(offset)?,
        address: row.get(offset + 1)?,
        postal_code: row.get(offset + 2)?,
        country: row.get(offset + 3)?,
        region: row.get(offset + 4)?,
        email: row.get(offset + 5)?,
        email_cc: row.get(offset + 6)?,
        phone: row.get(offset + 7)?,
        mobile: row.get(offset + 8)?,
        tax_id: row.get(offset + 9)?,
        tax_registration_type: row.get(offset + 10)?,
    })
}
fn profile_in(conn: &Connection, company_id: i64, ledger_id: i64) -> Result<Option<LedgerProfile>> {
    Ok(conn.query_row("SELECT mailing_name, address, postal_code, country, region, email, email_cc, phone, mobile, tax_id, tax_registration_type FROM ledger_profiles WHERE company_id = ?1 AND ledger_id = ?2", params![company_id, ledger_id], |r| profile_from_row(r, 0)).optional()?)
}
pub(crate) fn insert_new_profile(
    conn: &Connection,
    company_id: i64,
    ledger_id: i64,
    profile: &LedgerProfile,
) -> Result<()> {
    if *profile == LedgerProfile::default() {
        return Ok(());
    }
    conn.execute("INSERT INTO ledger_profiles(ledger_id, company_id, mailing_name, address, postal_code, country, region, email, email_cc, phone, mobile, tax_id, tax_registration_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)", params![ledger_id, company_id, profile.mailing_name, profile.address, profile.postal_code, profile.country, profile.region, profile.email, profile.email_cc, profile.phone, profile.mobile, profile.tax_id, profile.tax_registration_type])?;
    Ok(())
}
impl Store {
    pub fn ledger_profile(&self, company_id: i64, ledger_id: i64) -> Result<LedgerProfile> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ledgers WHERE company_id = ?1 AND id = ?2)",
            params![company_id, ledger_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Ledger not found in the selected company".into(),
            ));
        }
        Ok(profile_in(&self.conn, company_id, ledger_id)?.unwrap_or_default())
    }
    pub fn save_ledger_profile(
        &mut self,
        company_id: i64,
        ledger_id: i64,
        profile: &LedgerProfile,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ledgers WHERE company_id = ?1 AND id = ?2)",
            params![company_id, ledger_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Ledger not found in the selected company".into(),
            ));
        }
        if let Some(old) = profile_in(&tx, company_id, ledger_id)? {
            if old == *profile {
                tx.commit()?;
                return Ok(());
            }
            tx.execute("INSERT INTO ledger_profile_revisions(ledger_id, company_id, mailing_name, address, postal_code, country, region, email, email_cc, phone, mobile, tax_id, tax_registration_type) SELECT ledger_id, company_id, mailing_name, address, postal_code, country, region, email, email_cc, phone, mobile, tax_id, tax_registration_type FROM ledger_profiles WHERE company_id = ?1 AND ledger_id = ?2", params![company_id, ledger_id])?;
        }
        tx.execute("INSERT INTO ledger_profiles(ledger_id, company_id, mailing_name, address, postal_code, country, region, email, email_cc, phone, mobile, tax_id, tax_registration_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) ON CONFLICT(ledger_id) DO UPDATE SET mailing_name=excluded.mailing_name, address=excluded.address, postal_code=excluded.postal_code, country=excluded.country, region=excluded.region, email=excluded.email, email_cc=excluded.email_cc, phone=excluded.phone, mobile=excluded.mobile, tax_id=excluded.tax_id, tax_registration_type=excluded.tax_registration_type", params![ledger_id, company_id, profile.mailing_name, profile.address, profile.postal_code, profile.country, profile.region, profile.email, profile.email_cc, profile.phone, profile.mobile, profile.tax_id, profile.tax_registration_type])?;
        tx.commit()?;
        Ok(())
    }
    pub fn ledger_profile_revisions(
        &self,
        company_id: i64,
        ledger_id: i64,
    ) -> Result<Vec<ProfileRevision>> {
        let mut stmt = self.conn.prepare("SELECT changed_at, mailing_name, address, postal_code, country, region, email, email_cc, phone, mobile, tax_id, tax_registration_type FROM ledger_profile_revisions WHERE company_id = ?1 AND ledger_id = ?2 ORDER BY id DESC")?;
        let rows = stmt
            .query_map(params![company_id, ledger_id], |r| {
                Ok(ProfileRevision {
                    changed_at: r.get(0)?,
                    profile: profile_from_row(r, 1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
