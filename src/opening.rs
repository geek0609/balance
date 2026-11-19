//! Opening balances are represented by balanced vouchers against a visible
//! opening-difference ledger. Changes retain the earlier voucher revision.
use crate::{
    check_ledgers, parse_date, required, validate_voucher, Category, Error, Posting, Result, Store,
    VoucherKind,
};
use rusqlite::{params, Connection, OptionalExtension};

const DIFFERENCE_NAME: &str = "Opening Balance Difference";
pub(crate) fn is_control_name(name: &str) -> bool {
    name.trim().to_lowercase() == DIFFERENCE_NAME.to_lowercase()
}
#[derive(Debug, Clone)]
pub struct OpeningBalance {
    pub date: String,
    pub amount: i64,
    pub voucher_id: i64,
    pub on_account: bool,
}
#[derive(Debug, Clone)]
pub struct OpeningClear {
    pub original_voucher_id: i64,
    pub clearing_voucher_id: i64,
    pub date: String,
    pub reason: String,
    pub on_account: bool,
}

pub(crate) fn control_id(conn: &Connection, company: i64) -> Result<Option<i64>> {
    Ok(conn.query_row("SELECT c.opening_difference_ledger_id FROM company_controls c JOIN ledgers l ON l.id = c.opening_difference_ledger_id AND l.company_id = c.company_id WHERE c.company_id = ?1", [company], |r| r.get(0)).optional()?)
}
pub(crate) fn check_no_control_postings(
    conn: &Connection,
    company: i64,
    postings: &[Posting],
) -> Result<()> {
    if control_id(conn, company)?.is_some_and(|id| postings.iter().any(|p| p.ledger_id == id)) {
        return Err(Error::Validation(
            "The opening balance difference ledger is maintained automatically".into(),
        ));
    }
    Ok(())
}
pub(crate) fn ensure_control(conn: &Connection, company: i64) -> Result<i64> {
    if let Some(id) = control_id(conn, company)? {
        return Ok(id);
    }
    let mut stmt = conn.prepare("SELECT name FROM ledgers WHERE company_id = ?1")?;
    for name in stmt.query_map([company], |r| r.get::<_, String>(0))? {
        if is_control_name(&name?) {
            return Err(Error::Validation(
                "The reserved opening difference ledger name is already in use".into(),
            ));
        }
    }
    drop(stmt);
    let group: i64 = conn.query_row("SELECT id FROM account_groups WHERE company_id = ?1 AND name = 'Equity' AND parent_id IS NULL", [company], |r| r.get(0))?;
    conn.execute(
        "INSERT INTO ledgers(company_id, name, category, group_id) VALUES (?1, ?2, 'Equity', ?3)",
        params![company, DIFFERENCE_NAME, group],
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO company_controls(company_id, opening_difference_ledger_id) VALUES (?1, ?2)",
        params![company, id],
    )?;
    Ok(id)
}
pub(crate) fn migrate_on_account(conn: &Connection) -> Result<()> {
    for table in ["opening_balances", "opening_balance_clears"] {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'on_account')",
            [table],
            |row| row.get(0),
        )?;
        if !present {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN on_account INTEGER NOT NULL DEFAULT 0
                 CHECK(on_account IN (0, 1));",
            ))?;
        }
    }
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS opening_clear_preserves_on_account
         BEFORE INSERT ON opening_balance_clears
         WHEN EXISTS (
             SELECT 1 FROM opening_balances b
             WHERE b.company_id = NEW.company_id AND b.ledger_id = NEW.ledger_id
                 AND b.voucher_id = NEW.original_voucher_id AND b.on_account != NEW.on_account
         )
         BEGIN
             SELECT RAISE(ABORT, 'Opening balance clearing must preserve its on-account classification');
         END;",
    )?;
    Ok(())
}

pub(crate) fn save_opening_in_transaction(
    conn: &Connection,
    company: i64,
    ledger: i64,
    date: &str,
    amount: i64,
    on_account: bool,
) -> Result<i64> {
    let parsed_date = parse_date(date)?;
    if on_account
        && !matches!(
            crate::inventory::ledger_category(conn, company, ledger)?,
            Category::Asset | Category::Liability
        )
    {
        return Err(Error::Validation(
            "An opening balance on account requires an asset or liability ledger".into(),
        ));
    }
    crate::opening_bills::check_aggregate_opening(conn, company, ledger, parsed_date, on_account)?;
    let date = parsed_date.format("%Y-%m-%d").to_string();
    let last_clear: Option<String> = conn.query_row("SELECT MAX(v.date) FROM opening_balance_clears c JOIN vouchers v ON v.id = c.clearing_voucher_id AND v.company_id = c.company_id WHERE c.company_id = ?1 AND c.ledger_id = ?2", params![company, ledger], |r| r.get(0))?;
    if last_clear
        .as_deref()
        .is_some_and(|cleared| date.as_str() < cleared)
    {
        return Err(Error::Validation(
            "Opening date must not precede the last clearing entry".into(),
        ));
    }
    if amount == 0 || amount == i64::MIN {
        return Err(Error::Validation(
            "Opening balance must be a non-zero supported amount".into(),
        ));
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ledgers WHERE id = ?1 AND company_id = ?2)",
        params![ledger, company],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(Error::Validation(
            "Ledger not found in the selected company".into(),
        ));
    }
    let control = ensure_control(conn, company)?;
    if ledger == control {
        return Err(Error::Validation(
            "The difference ledger is maintained automatically".into(),
        ));
    }
    let postings = [
        Posting {
            ledger_id: ledger,
            amount,
        },
        Posting {
            ledger_id: control,
            amount: -amount,
        },
    ];
    validate_voucher(&date, &postings)?;
    check_ledgers(conn, company, &postings)?;
    let existing: Option<(i64, bool)> = conn
        .query_row(
            "SELECT voucher_id, on_account FROM opening_balances WHERE company_id = ?1 AND ledger_id = ?2",
            params![company, ledger],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let voucher_id = if let Some((id, existing_on_account)) = existing {
        if existing_on_account != on_account {
            return Err(Error::Validation(
                "Clear the current opening balance before changing its type".into(),
            ));
        }
        crate::banking::ensure_no_bank_clearance(conn, company, id)?;
        conn.execute("INSERT INTO voucher_revisions(voucher_id, company_id, date, narration, kind, reference) SELECT id, company_id, date, narration, kind, reference FROM vouchers WHERE id = ?1 AND company_id = ?2", params![id, company])?;
        let revision = conn.last_insert_rowid();
        conn.execute("INSERT INTO revision_postings(revision_id, ledger_id, amount) SELECT ?1, ledger_id, amount FROM postings WHERE voucher_id = ?2 ORDER BY id", params![revision, id])?;
        conn.execute(
            "UPDATE vouchers SET date = ?1 WHERE id = ?2 AND company_id = ?3",
            params![date, id, company],
        )?;
        conn.execute("DELETE FROM postings WHERE voucher_id = ?1", [id])?;
        conn.execute("UPDATE opening_balances SET date = ?1, amount = ?2 WHERE company_id = ?3 AND ledger_id = ?4", params![date, amount, company, ledger])?;
        id
    } else {
        let narration = if on_account {
            "Opening balance on account"
        } else {
            "Opening balance"
        };
        conn.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, '', ?3, ?4)", params![company, VoucherKind::Journal.as_str(), date, narration])?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO opening_balances(company_id, ledger_id, voucher_id, date, amount, on_account)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![company, ledger, id, date, amount, on_account],
        )?;
        id
    };
    for posting in postings {
        conn.execute(
            "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
            params![voucher_id, posting.ledger_id, posting.amount],
        )?;
    }
    Ok(voucher_id)
}
pub(crate) fn opening_on_account_as_of(
    conn: &Connection,
    company: i64,
    date: chrono::NaiveDate,
) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.name, b.amount
         FROM opening_balances b JOIN ledgers l ON l.id = b.ledger_id AND l.company_id = b.company_id
         WHERE b.company_id = ?1 AND b.on_account = 1 AND b.date <= ?2
         UNION ALL
         SELECT l.id, l.name, p.amount
         FROM opening_balance_clears c
         JOIN ledgers l ON l.id = c.ledger_id AND l.company_id = c.company_id
         JOIN vouchers original ON original.id = c.original_voucher_id AND original.company_id = c.company_id
         JOIN vouchers clearing ON clearing.id = c.clearing_voucher_id AND clearing.company_id = c.company_id
         JOIN postings p ON p.voucher_id = original.id AND p.ledger_id = l.id
         WHERE c.company_id = ?1 AND c.on_account = 1 AND original.date <= ?2 AND clearing.date > ?2",
    )?;
    let rows = stmt
        .query_map(params![company, date.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

impl Store {
    /// Post a counter-entry and retain both vouchers and their link as history.
    pub fn clear_opening_balance(
        &mut self,
        company: i64,
        ledger: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Clearing reference")?;
        let reason = required(reason, "Clearing reason")?;
        let tx = self.conn.transaction()?;
        let (original, original_date, amount, on_account): (i64, String, i64, bool) = tx.query_row("SELECT voucher_id, date, amount, on_account FROM opening_balances WHERE company_id = ?1 AND ledger_id = ?2", params![company, ledger], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::Validation("No opening balance exists for this ledger".into()),
            other => Error::Database(other),
        })?;
        if date < parse_date(&original_date)? {
            return Err(Error::Validation(
                "Clearing date must not precede the opening balance".into(),
            ));
        }
        let control = control_id(&tx, company)?
            .ok_or_else(|| Error::Validation("Opening difference ledger is missing".into()))?;
        let inverse = amount
            .checked_neg()
            .ok_or_else(|| Error::Validation("Opening amount cannot be cleared".into()))?;
        let mut stmt =
            tx.prepare("SELECT ledger_id, amount FROM postings WHERE voucher_id = ?1 ORDER BY id")?;
        let postings = stmt
            .query_map([original], |r| {
                Ok(Posting {
                    ledger_id: r.get(0)?,
                    amount: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let voucher_date: String = tx.query_row(
            "SELECT date FROM vouchers WHERE id = ?1 AND company_id = ?2",
            params![original, company],
            |r| r.get(0),
        )?;
        if parse_date(&voucher_date)? != parse_date(&original_date)?
            || postings.len() != 2
            || !postings
                .iter()
                .any(|p| p.ledger_id == ledger && p.amount == amount)
            || !postings
                .iter()
                .any(|p| p.ledger_id == control && p.amount == inverse)
        {
            return Err(Error::Validation(
                "Opening balance postings are inconsistent".into(),
            ));
        }
        let reversed = [
            Posting {
                ledger_id: ledger,
                amount: inverse,
            },
            Posting {
                ledger_id: control,
                amount,
            },
        ];
        let date = date.format("%Y-%m-%d").to_string();
        validate_voucher(&date, &reversed)?;
        check_ledgers(&tx, company, &reversed)?;
        tx.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company, VoucherKind::Journal.as_str(), reference, date, format!("Clearing of opening balance voucher {original}: {reason}")])?;
        let clearing = tx.last_insert_rowid();
        for posting in reversed {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![clearing, posting.ledger_id, posting.amount],
            )?;
        }
        tx.execute(
            "INSERT INTO opening_balance_clears(company_id, ledger_id, original_voucher_id, clearing_voucher_id, reason, on_account)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![company, ledger, original, clearing, reason, on_account],
        )?;
        tx.execute(
            "DELETE FROM opening_balances WHERE company_id = ?1 AND ledger_id = ?2",
            params![company, ledger],
        )?;
        tx.commit()?;
        Ok(clearing)
    }

    pub fn opening_clear_history(&self, company: i64, ledger: i64) -> Result<Vec<OpeningClear>> {
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
        let mut stmt = self.conn.prepare("SELECT c.original_voucher_id, c.clearing_voucher_id, v.date, c.reason, c.on_account FROM opening_balance_clears c JOIN vouchers v ON v.id = c.clearing_voucher_id AND v.company_id = c.company_id WHERE c.company_id = ?1 AND c.ledger_id = ?2 ORDER BY c.id DESC")?;
        let rows = stmt
            .query_map(params![company, ledger], |r| {
                Ok(OpeningClear {
                    original_voucher_id: r.get(0)?,
                    clearing_voucher_id: r.get(1)?,
                    date: r.get(2)?,
                    reason: r.get(3)?,
                    on_account: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn opening_balance(&self, company: i64, ledger: i64) -> Result<Option<OpeningBalance>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ledgers WHERE id = ?1 AND company_id = ?2)",
            params![ledger, company],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Ledger not found in the selected company".into(),
            ));
        }
        Ok(self.conn.query_row(
            "SELECT date, amount, voucher_id, on_account FROM opening_balances WHERE company_id = ?1 AND ledger_id = ?2",
            params![company, ledger],
            |r| Ok(OpeningBalance {
                date: r.get(0)?,
                amount: r.get(1)?,
                voucher_id: r.get(2)?,
                on_account: r.get(3)?,
            }),
        ).optional()?)
    }
    pub fn set_opening_balance(
        &mut self,
        company: i64,
        ledger: i64,
        date: &str,
        amount: i64,
    ) -> Result<i64> {
        self.set_opening_balance_mode(company, ledger, date, amount, false)
    }

    /// Record the portion of the opening balance that is unallocated to bills.
    pub fn set_opening_on_account_balance(
        &mut self,
        company: i64,
        ledger: i64,
        date: &str,
        amount: i64,
    ) -> Result<i64> {
        self.set_opening_balance_mode(company, ledger, date, amount, true)
    }

    fn set_opening_balance_mode(
        &mut self,
        company: i64,
        ledger: i64,
        date: &str,
        amount: i64,
        on_account: bool,
    ) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let id = save_opening_in_transaction(&tx, company, ledger, date, amount, on_account)?;
        tx.commit()?;
        Ok(id)
    }
}
