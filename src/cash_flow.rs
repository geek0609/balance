//! Cash movement reporting with explicit ledger and voucher classifications.
use crate::inventory::ledger_category;
use crate::{parse_date, required, Category, Error, Result, Store, VoucherKind};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE cash_flow_ledger_rules(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            role TEXT NOT NULL CHECK(role IN ('Cash', 'Operating', 'Investing', 'Financing', 'Unclassified')),
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX cash_flow_ledger_rules_company ON cash_flow_ledger_rules(company_id, ledger_id, id);
        CREATE TABLE cash_flow_voucher_rules(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            voucher_id INTEGER NOT NULL REFERENCES vouchers(id),
            classification TEXT CHECK(classification IN ('Operating', 'Investing', 'Financing', 'Unclassified')),
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX cash_flow_voucher_rules_company ON cash_flow_voucher_rules(company_id, voucher_id, id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CashClass {
    Operating,
    Investing,
    Financing,
    Unclassified,
}
impl CashClass {
    pub const ALL: [Self; 4] = [
        Self::Operating,
        Self::Investing,
        Self::Financing,
        Self::Unclassified,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Operating => "Operating",
            Self::Investing => "Investing",
            Self::Financing => "Financing",
            Self::Unclassified => "Unclassified",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.label() == value)
            .ok_or_else(|| Error::Validation("Unknown cash flow classification".into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CashRole {
    Cash,
    Counterpart(CashClass),
}
impl CashRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cash => "Cash or bank",
            Self::Counterpart(class) => class.label(),
        }
    }

    fn stored(self) -> &'static str {
        match self {
            Self::Cash => "Cash",
            Self::Counterpart(class) => class.label(),
        }
    }

    fn parse(value: &str) -> Result<Self> {
        if value == "Cash" {
            Ok(Self::Cash)
        } else {
            Ok(Self::Counterpart(CashClass::parse(value)?))
        }
    }
}

#[derive(Debug, Clone)]
pub struct CashLedgerRule {
    pub ledger_id: i64,
    pub role: CashRole,
    pub reason: String,
    pub recorded_at: String,
}
#[derive(Debug, Clone)]
pub struct CashVoucherRule {
    pub class: Option<CashClass>,
    pub reason: String,
    pub recorded_at: String,
}
#[derive(Debug, Clone)]
pub struct CashFlowEntry {
    pub voucher_id: i64,
    pub date: String,
    pub kind: VoucherKind,
    pub reference: String,
    pub class: Option<CashClass>,
    pub counterpart: Option<String>,
    pub amount: i128,
    pub explanation: &'static str,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct CashTotals {
    pub inflows: i128,
    pub outflows: i128,
}
impl CashTotals {
    pub fn net(self) -> i128 {
        self.inflows - self.outflows
    }
}
#[derive(Debug, Clone)]
pub struct CashFlowReport {
    pub opening: i128,
    pub closing: i128,
    pub totals: BTreeMap<CashClass, CashTotals>,
    pub entries: Vec<CashFlowEntry>,
}

impl Store {
    pub fn save_cash_ledger_rule(
        &mut self,
        company: i64,
        ledger: i64,
        role: CashRole,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason for the ledger classification")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let category = ledger_category(&tx, company, ledger)?;
        if role == CashRole::Cash && category != Category::Asset {
            return Err(Error::Validation("A cash ledger must be an asset".into()));
        }
        let previous: Option<String> = tx.query_row(
            "SELECT role FROM cash_flow_ledger_rules WHERE company_id = ?1 AND ledger_id = ?2 ORDER BY id DESC LIMIT 1",
            params![company, ledger], |row| row.get(0),
        ).optional()?;
        if previous.as_deref() == Some(role.stored()) {
            return Err(Error::Validation(
                "This ledger classification is already saved".into(),
            ));
        }
        tx.execute(
            "INSERT INTO cash_flow_ledger_rules(company_id, ledger_id,role,reason) VALUES (?1,?2,?3,?4)",
            params![company, ledger, role.stored(), reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn cash_ledger_rules(
        &self,
        company: i64,
        ledger: Option<i64>,
    ) -> Result<Vec<CashLedgerRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT ledger_id, role, reason, recorded_at FROM cash_flow_ledger_rules
             WHERE company_id = ?1 AND (?2 IS NULL OR ledger_id = ?2) ORDER BY id DESC",
        )?;
        let mut rows = stmt.query(params![company, ledger])?;
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        while let Some(row) = rows.next()? {
            let ledger_id = row.get(0)?;
            if ledger.is_none() && !seen.insert(ledger_id) {
                continue;
            }
            result.push(CashLedgerRule {
                ledger_id,
                role: CashRole::parse(&row.get::<_, String>(1)?)?,
                reason: row.get(2)?,
                recorded_at: row.get(3)?,
            });
        }
        Ok(result)
    }

    pub fn save_cash_voucher_rule(
        &mut self,
        company: i64,
        voucher: i64,
        class: Option<CashClass>,
        reason: &str,
    ) -> Result<()> {
        let reason = required(reason, "Reason for the voucher classification")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM vouchers WHERE company_id = ?1 AND id = ?2)",
            params![company, voucher],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Voucher not found in this company".into(),
            ));
        }
        let previous: Option<Option<String>> = tx.query_row(
            "SELECT classification FROM cash_flow_voucher_rules WHERE company_id = ?1 AND voucher_id = ?2 ORDER BY id DESC LIMIT 1",
            params![company, voucher], |row| row.get(0),
        ).optional()?;
        let stored = class.map(CashClass::label);
        if previous
            .as_ref()
            .is_some_and(|value| value.as_deref() == stored)
        {
            return Err(Error::Validation(
                "This voucher classification is already in effect".into(),
            ));
        }
        tx.execute(
            "INSERT INTO cash_flow_voucher_rules(company_id,voucher_id,classification,reason) VALUES (?1,?2,?3,?4)",
            params![company, voucher, stored, reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn cash_voucher_rule_history(
        &self,
        company: i64,
        voucher: i64,
    ) -> Result<Vec<CashVoucherRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT classification,reason,recorded_at FROM cash_flow_voucher_rules
             WHERE company_id = ?1 AND voucher_id = ?2 ORDER BY id DESC",
        )?;
        let mut rows = stmt.query(params![company, voucher])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let code: Option<String> = row.get(0)?;
            result.push(CashVoucherRule {
                class: code.as_deref().map(CashClass::parse).transpose()?,
                reason: row.get(1)?,
                recorded_at: row.get(2)?,
            });
        }
        Ok(result)
    }

    pub fn cash_flow_report(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<CashFlowReport> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(start), Some(end)) if start > end) {
            return Err(Error::Validation(
                "Cash flow start date cannot follow its end date".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let rules = self
            .cash_ledger_rules(company, None)?
            .into_iter()
            .map(|row| (row.ledger_id, row.role))
            .collect::<HashMap<_, _>>();
        if !rules.values().any(|role| *role == CashRole::Cash) {
            return Err(Error::Validation(
                "Mark at least one cash or bank ledger first".into(),
            ));
        }
        let ledgers = self
            .ledgers(company)?
            .into_iter()
            .map(|row| (row.id, row.name))
            .collect::<HashMap<_, _>>();
        let mut overrides = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT voucher_id, classification FROM cash_flow_voucher_rules WHERE company_id = ?1 ORDER BY id",
            )?;
            let mut rows = stmt.query([company])?;
            while let Some(row) = rows.next()? {
                let voucher: i64 = row.get(0)?;
                let value: Option<String> = row.get(1)?;
                overrides.insert(voucher, value.as_deref().map(CashClass::parse).transpose()?);
            }
        }
        let mut reversal_sources = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT reversal_voucher_id, original_voucher_id FROM voucher_reversals WHERE company_id = ?1
                 UNION ALL SELECT reversal_voucher_id, settlement_voucher_id FROM settlement_reversals WHERE company_id = ?1
                 UNION ALL SELECT clearing_voucher_id, original_voucher_id FROM opening_balance_clears WHERE company_id = ?1
                 UNION ALL SELECT r.reversal_voucher_id, i.voucher_id FROM invoice_reversals r
                              JOIN invoices i ON i.id = r.invoice_id WHERE r.company_id = ?1
                 UNION ALL SELECT reversal_voucher_id, voucher_id FROM invoice_credit_notes WHERE company_id = ?1 AND reversal_voucher_id IS NOT NULL
                 UNION ALL SELECT reversal_voucher_id, voucher_id FROM invoice_credit_refunds WHERE company_id = ?1 AND reversal_voucher_id IS NOT NULL
                 UNION ALL SELECT reversal_voucher_id, voucher_id FROM party_advances WHERE company_id = ?1 AND reversal_voucher_id IS NOT NULL
                 UNION ALL SELECT reversal_voucher_id, voucher_id FROM payroll_accruals WHERE company_id = ?1 AND reversal_voucher_id IS NOT NULL
                 UNION ALL SELECT reversal_voucher_id, voucher_id FROM payroll_payments WHERE company_id = ?1 AND reversal_voucher_id IS NOT NULL",
            )?;
            let mut rows = stmt.query([company])?;
            while let Some(row) = rows.next()? {
                reversal_sources.insert(row.get::<_, i64>(0)?, row.get::<_, i64>(1)?);
            }
        }
        let mut report = CashFlowReport {
            opening: 0,
            closing: 0,
            totals: BTreeMap::new(),
            entries: Vec::new(),
        };
        for voucher in self.vouchers(company)? {
            let date = parse_date(&voucher.date)?;
            if end.is_some_and(|end| date > end) {
                continue;
            }
            let mut cash = 0_i128;
            let mut has_cash = false;
            let mut counterparts = BTreeMap::<i64, i128>::new();
            for posting in voucher.postings {
                let amount = i128::from(posting.amount);
                if rules.get(&posting.ledger_id) == Some(&CashRole::Cash) {
                    cash += amount;
                    has_cash = true;
                } else {
                    *counterparts.entry(posting.ledger_id).or_default() += amount;
                }
            }
            if !has_cash {
                continue;
            }
            report.closing += cash;
            if start.is_some_and(|start| date < start) {
                report.opening += cash;
                continue;
            }
            let mut add = |class, counterpart, amount: i128, explanation| {
                if let Some(class) = class {
                    let totals = report.totals.entry(class).or_default();
                    totals.inflows += amount.max(0);
                    totals.outflows += (-amount).max(0);
                }
                report.entries.push(CashFlowEntry {
                    voucher_id: voucher.id,
                    date: voucher.date.clone(),
                    kind: voucher.kind,
                    reference: voucher.reference.clone(),
                    class,
                    counterpart,
                    amount,
                    explanation,
                });
            };
            if cash == 0 {
                add(None, None, 0, "Net-zero cash movement");
                continue;
            }
            let override_rule = overrides.get(&voucher.id).or_else(|| {
                reversal_sources
                    .get(&voucher.id)
                    .and_then(|id| overrides.get(id))
            });
            if let Some(&Some(class)) = override_rule {
                add(Some(class), None, cash, "Voucher classification");
                continue;
            }
            counterparts.retain(|_, amount| *amount != 0);
            if counterparts
                .values()
                .any(|amount| amount.signum() == cash.signum())
            {
                add(
                    Some(CashClass::Unclassified),
                    None,
                    cash,
                    "Mixed counterpart postings",
                );
                continue;
            }
            for (id, amount) in counterparts {
                let class = match rules.get(&id) {
                    Some(CashRole::Counterpart(class)) => *class,
                    _ => CashClass::Unclassified,
                };
                add(
                    Some(class),
                    ledgers.get(&id).cloned(),
                    -amount,
                    "Counterpart ledger",
                );
            }
        }
        let movement: i128 = report.totals.values().map(|total| total.net()).sum();
        if report.opening + movement != report.closing {
            return Err(Error::Validation(
                "Cash flow does not reconcile to the closing balance".into(),
            ));
        }
        tx.commit()?;
        Ok(report)
    }
}
