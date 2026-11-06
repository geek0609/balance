//! Optional business details and informational accounting dates.
use crate::{parse_date, Error, Result, Store};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompanyProfile {
    pub address: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
    pub email: String,
    pub phone: String,
    pub tax_id: String,
    pub registration_type: String,
    pub financial_year_start: String,
    pub books_start: String,
}

#[derive(Debug, Clone)]
pub struct CompanyProfileRevision {
    pub changed_at: String,
    pub profile: CompanyProfile,
}

fn profile_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<CompanyProfile> {
    Ok(CompanyProfile {
        address: row.get(offset)?,
        region: row.get(offset + 1)?,
        postal_code: row.get(offset + 2)?,
        country: row.get(offset + 3)?,
        email: row.get(offset + 4)?,
        phone: row.get(offset + 5)?,
        tax_id: row.get(offset + 6)?,
        registration_type: row.get(offset + 7)?,
        financial_year_start: row.get(offset + 8)?,
        books_start: row.get(offset + 9)?,
    })
}

fn normalize_date(value: &str) -> Result<String> {
    if value.trim().is_empty() {
        Ok(String::new())
    } else {
        Ok(parse_date(value)?.format("%Y-%m-%d").to_string())
    }
}

impl Store {
    pub fn company_profile(&self, company: i64) -> Result<CompanyProfile> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM companies WHERE id = ?1)",
            [company],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation("Company not found".into()));
        }
        Ok(self.conn.query_row("SELECT address, region, postal_code, country, email, phone, tax_id, registration_type, financial_year_start, books_start FROM company_profiles WHERE company_id = ?1", [company], |r| profile_from_row(r, 0)).optional()?.unwrap_or_default())
    }

    pub fn save_company_profile(&mut self, company: i64, profile: &CompanyProfile) -> Result<()> {
        let mut profile = profile.clone();
        profile.financial_year_start = normalize_date(&profile.financial_year_start)?;
        profile.books_start = normalize_date(&profile.books_start)?;
        let old = self.company_profile(company)?;
        if old == profile {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO company_profile_revisions(company_id, address, region, postal_code, country, email, phone, tax_id, registration_type, financial_year_start, books_start) SELECT company_id, address, region, postal_code, country, email, phone, tax_id, registration_type, financial_year_start, books_start FROM company_profiles WHERE company_id = ?1", [company])?;
        tx.execute("INSERT INTO company_profiles(company_id, address, region, postal_code, country, email, phone, tax_id, registration_type, financial_year_start, books_start) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT(company_id) DO UPDATE SET address=excluded.address, region=excluded.region, postal_code=excluded.postal_code, country=excluded.country, email=excluded.email, phone=excluded.phone, tax_id=excluded.tax_id, registration_type=excluded.registration_type, financial_year_start=excluded.financial_year_start, books_start=excluded.books_start", params![company, profile.address, profile.region, profile.postal_code, profile.country, profile.email, profile.phone, profile.tax_id, profile.registration_type, profile.financial_year_start, profile.books_start])?;
        tx.commit()?;
        Ok(())
    }

    pub fn company_profile_revisions(&self, company: i64) -> Result<Vec<CompanyProfileRevision>> {
        self.company_profile(company)?;
        let mut stmt = self.conn.prepare("SELECT changed_at, address, region, postal_code, country, email, phone, tax_id, registration_type, financial_year_start, books_start FROM company_profile_revisions WHERE company_id = ?1 ORDER BY id DESC")?;
        let rows = stmt
            .query_map([company], |r| {
                Ok(CompanyProfileRevision {
                    changed_at: r.get(0)?,
                    profile: profile_from_row(r, 1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
