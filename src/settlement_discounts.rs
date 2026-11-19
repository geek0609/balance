//! Noncash discounts recorded alongside a bill settlement.
use crate::inventory::{ledger_category, InvoiceKind};
use crate::{params, Category, Connection, Error, Posting, Result};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementDiscount {
    pub ledger_id: i64,
    pub amount: i64,
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settlement_discounts(
            voucher_id INTEGER NOT NULL REFERENCES vouchers(id),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            PRIMARY KEY(voucher_id, ledger_id)
        );",
    )?;
    Ok(())
}

pub(crate) fn postings(
    conn: &Connection,
    company: i64,
    kind: InvoiceKind,
    bank: i64,
    party: i64,
    settled: i128,
    other_amount: i128,
    discounts: &[SettlementDiscount],
) -> Result<Vec<Posting>> {
    let total = i64::try_from(settled + other_amount)
        .map_err(|_| Error::Validation("Receipt or payment total is too large".into()))?;
    let mut ledgers = HashSet::from([bank, party]);
    let mut discounted = 0_i128;
    let direction = if kind == InvoiceKind::Sale { 1 } else { -1 };
    let mut postings = Vec::with_capacity(2 + discounts.len());
    for discount in discounts {
        if discount.amount <= 0 || !ledgers.insert(discount.ledger_id) {
            return Err(Error::Validation(
                "Each discount needs a distinct ledger and a positive amount".into(),
            ));
        }
        if !matches!(
            ledger_category(conn, company, discount.ledger_id)?,
            Category::Income | Category::Expense
        ) {
            return Err(Error::Validation(
                "Select an income or expense ledger for the discount".into(),
            ));
        }
        discounted += i128::from(discount.amount);
    }
    if discounted > settled {
        return Err(Error::Validation(
            "Discounts cannot exceed the amount allocated to bills".into(),
        ));
    }
    let cash = i64::try_from(i128::from(total) - discounted)
        .ok()
        .filter(|amount| *amount > 0)
        .ok_or_else(|| Error::Validation("Cash or bank amount must be positive".into()))?;
    postings.push(Posting {
        ledger_id: bank,
        amount: direction * cash,
    });
    postings.push(Posting {
        ledger_id: party,
        amount: -direction * total,
    });
    postings.extend(discounts.iter().map(|discount| Posting {
        ledger_id: discount.ledger_id,
        amount: direction * discount.amount,
    }));
    Ok(postings)
}

pub(crate) fn save(
    conn: &Connection,
    voucher: i64,
    discounts: &[SettlementDiscount],
) -> Result<()> {
    for discount in discounts {
        conn.execute(
            "INSERT INTO settlement_discounts(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
            params![voucher, discount.ledger_id, discount.amount],
        )?;
    }
    Ok(())
}

pub(crate) fn read(conn: &Connection, voucher: i64) -> Result<Vec<SettlementDiscount>> {
    let mut stmt = conn.prepare_cached(
        "SELECT ledger_id, amount FROM settlement_discounts WHERE voucher_id = ?1 ORDER BY ledger_id",
    )?;
    let rows = stmt.query_map([voucher], |row| {
        Ok(SettlementDiscount {
            ledger_id: row.get(0)?,
            amount: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
