//! Payroll accruals from explicitly entered amounts.
use crate::inventory::ledger_category;
use crate::{
    check_ledgers, parse_date, required, validate_voucher, Category, Error, Posting, Result, Store,
    VoucherKind,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE payroll_employees(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            code TEXT NOT NULL,
            code_key TEXT NOT NULL,
            name TEXT NOT NULL,
            department TEXT NOT NULL,
            title TEXT NOT NULL,
            start_date TEXT,
            end_date TEXT,
            payable_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            UNIQUE(company_id, code_key)
        );
        CREATE TABLE payroll_heads(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            name TEXT NOT NULL,
            name_key TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('Earning', 'Deduction', 'Employer contribution')),
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            liability_ledger_id INTEGER REFERENCES ledgers(id),
            CHECK((kind = 'Employer contribution') = (liability_ledger_id IS NOT NULL)),
            UNIQUE(company_id, name_key)
        );
        CREATE TABLE payroll_accruals(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            employee_id INTEGER NOT NULL REFERENCES payroll_employees(id),
            voucher_id INTEGER NOT NULL UNIQUE REFERENCES vouchers(id),
            period_start TEXT NOT NULL,
            period_end TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            gross INTEGER NOT NULL CHECK(gross > 0),
            deductions INTEGER NOT NULL CHECK(deductions >= 0),
            net INTEGER NOT NULL CHECK(net >= 0 AND net = gross - deductions),
            employer_contributions INTEGER NOT NULL CHECK(employer_contributions >= 0),
            UNIQUE(company_id, employee_id, period_start, period_end),
            UNIQUE(company_id, reference_key)
        );
        CREATE INDEX payroll_accruals_company ON payroll_accruals(company_id, id);
        CREATE TABLE payroll_lines(
            id INTEGER PRIMARY KEY,
            accrual_id INTEGER NOT NULL REFERENCES payroll_accruals(id),
            head_id INTEGER NOT NULL REFERENCES payroll_heads(id),
            amount INTEGER NOT NULL CHECK(amount > 0),
            UNIQUE(accrual_id, head_id)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct EmployeeDraft {
    pub code: String,
    pub name: String,
    pub department: String,
    pub title: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub payable_ledger_id: i64,
}

#[derive(Debug, Clone)]
pub struct Employee {
    pub id: i64,
    pub details: EmployeeDraft,
    pub payable_ledger: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayHeadKind {
    Earning,
    Deduction,
    EmployerContribution,
}
impl PayHeadKind {
    pub const ALL: [Self; 3] = [Self::Earning, Self::Deduction, Self::EmployerContribution];
    pub fn label(self) -> &'static str {
        match self {
            Self::Earning => "Earning",
            Self::Deduction => "Deduction",
            Self::EmployerContribution => "Employer contribution",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.label() == value)
            .ok_or_else(|| Error::Validation("Unknown payroll head type".into()))
    }
}

#[derive(Debug, Clone)]
pub struct PayHeadDraft {
    pub kind: PayHeadKind,
    pub name: String,
    pub ledger_id: i64,
    pub liability_ledger_id: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct PayHead {
    pub id: i64,
    pub details: PayHeadDraft,
    pub ledger: String,
    pub liability_ledger: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct PayAmount {
    pub head_id: i64,
    pub amount: i64,
}
#[derive(Debug, Clone)]
pub struct PayrollDraft {
    pub employee_id: i64,
    pub date: String,
    pub period_start: String,
    pub period_end: String,
    pub reference: String,
    pub narration: String,
    pub amounts: Vec<PayAmount>,
}
#[derive(Debug, Clone)]
pub struct PayrollAccrual {
    pub id: i64,
    pub employee_id: i64,
    pub code: String,
    pub employee: String,
    pub department: String,
    pub voucher_id: i64,
    pub date: String,
    pub period_start: String,
    pub period_end: String,
    pub reference: String,
    pub narration: String,
    pub gross: i64,
    pub deductions: i64,
    pub net: i64,
    pub employer_contributions: i64,
    pub paid: i128,
    pub outstanding: i128,
}

#[derive(Debug, Clone)]
pub struct PayrollLine {
    pub head: String,
    pub kind: PayHeadKind,
    pub amount: i64,
}

fn canonical_date(date: Option<&str>) -> Result<Option<String>> {
    Ok(date
        .map(parse_date)
        .transpose()?
        .map(|day| day.format("%Y-%m-%d").to_string()))
}

fn supported_amount(amount: i128) -> Result<i64> {
    i64::try_from(amount).map_err(|_| Error::Validation("Payroll amount is too large".into()))
}

impl Store {
    pub fn create_employee(&mut self, company: i64, draft: &EmployeeDraft) -> Result<i64> {
        let code = required(&draft.code, "Employee code")?;
        let name = required(&draft.name, "Employee name")?;
        let from = draft.start_date.as_deref().map(parse_date).transpose()?;
        let to = draft.end_date.as_deref().map(parse_date).transpose()?;
        if matches!((from, to), (Some(from), Some(to)) if from > to) {
            return Err(Error::Validation(
                "Employment end date cannot precede the start".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if ledger_category(&tx, company, draft.payable_ledger_id)? != Category::Liability {
            return Err(Error::Validation(
                "Employee payable ledger must be a liability".into(),
            ));
        }
        let wrote = tx.execute(
            "INSERT INTO payroll_employees(company_id, code, code_key, name, department, title, start_date, end_date, payable_ledger_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(company_id, code_key) DO NOTHING",
            params![company, code, code.to_lowercase(), name, draft.department.trim(), draft.title.trim(),
                canonical_date(draft.start_date.as_deref())?, canonical_date(draft.end_date.as_deref())?, draft.payable_ledger_id],
        )?;
        if wrote != 1 {
            return Err(Error::Validation(
                "This employee code already exists".into(),
            ));
        }
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn employees(&self, company: i64) -> Result<Vec<Employee>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id,e.code,e.name,e.department,e.title,e.start_date,e.end_date,e.payable_ledger_id,l.name
             FROM payroll_employees e JOIN ledgers l ON l.id = e.payable_ledger_id
             WHERE e.company_id = ?1 ORDER BY e.code, e.id",
        )?;
        let rows = stmt.query_map([company], |row| {
            Ok(Employee {
                id: row.get(0)?,
                details: EmployeeDraft {
                    code: row.get(1)?,
                    name: row.get(2)?,
                    department: row.get(3)?,
                    title: row.get(4)?,
                    start_date: row.get(5)?,
                    end_date: row.get(6)?,
                    payable_ledger_id: row.get(7)?,
                },
                payable_ledger: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_pay_head(&mut self, company: i64, draft: &PayHeadDraft) -> Result<i64> {
        let name = required(&draft.name, "Pay head name")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let category = ledger_category(&tx, company, draft.ledger_id)?;
        let valid = match (draft.kind, draft.liability_ledger_id) {
            (PayHeadKind::Earning, None) => category == Category::Expense,
            (PayHeadKind::Deduction, None) => category == Category::Liability,
            (PayHeadKind::EmployerContribution, Some(id)) => {
                category == Category::Expense
                    && ledger_category(&tx, company, id)? == Category::Liability
            }
            _ => false,
        };
        if !valid {
            return Err(Error::Validation(
                "Earnings require an expense ledger, deductions require a liability ledger, and employer contributions require one of each".into(),
            ));
        }
        let wrote = tx.execute(
            "INSERT INTO payroll_heads(company_id, name, name_key, kind, ledger_id, liability_ledger_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(company_id, name_key) DO NOTHING",
            params![company, name, name.to_lowercase(), draft.kind.label(), draft.ledger_id, draft.liability_ledger_id],
        )?;
        if wrote != 1 {
            return Err(Error::Validation(
                "This pay head name already exists".into(),
            ));
        }
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn pay_heads(&self, company: i64) -> Result<Vec<PayHead>> {
        let mut stmt = self.conn.prepare(
            "SELECT h.id, h.kind, h.name, h.ledger_id, h.liability_ledger_id, l.name, o.name
             FROM payroll_heads h JOIN ledgers l ON l.id = h.ledger_id
             LEFT JOIN ledgers o ON o.id = h.liability_ledger_id
             WHERE h.company_id = ?1 ORDER BY h.kind, h.name, h.id",
        )?;
        let mut rows = stmt.query([company])?;
        let mut heads = Vec::new();
        while let Some(row) = rows.next()? {
            heads.push(PayHead {
                id: row.get(0)?,
                details: PayHeadDraft {
                    kind: PayHeadKind::parse(&row.get::<_, String>(1)?)?,
                    name: row.get(2)?,
                    ledger_id: row.get(3)?,
                    liability_ledger_id: row.get(4)?,
                },
                ledger: row.get(5)?,
                liability_ledger: row.get(6)?,
            });
        }
        Ok(heads)
    }

    pub fn post_payroll_accrual(&mut self, company: i64, draft: &PayrollDraft) -> Result<i64> {
        let from = parse_date(&draft.period_start)?;
        let to = parse_date(&draft.period_end)?;
        let date = parse_date(&draft.date)?;
        let reference = required(&draft.reference, "Payroll reference")?;
        if from > to || date < from {
            return Err(Error::Validation(
                "The payroll period is invalid or follows the posting date".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (payable, hired, departed): (i64, Option<String>, Option<String>) = tx.query_row(
            "SELECT payable_ledger_id, start_date, end_date FROM payroll_employees WHERE company_id = ?1 AND id = ?2",
            params![company, draft.employee_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?.ok_or_else(|| Error::Validation("Employee not found in this company".into()))?;
        if hired
            .as_deref()
            .map(parse_date)
            .transpose()?
            .is_some_and(|start| start > to)
            || departed
                .as_deref()
                .map(parse_date)
                .transpose()?
                .is_some_and(|end| end < from)
        {
            return Err(Error::Validation(
                "Employee dates do not overlap this payroll period".into(),
            ));
        }
        let mut gross = 0_i128;
        let mut deductions = 0_i128;
        let mut employer = 0_i128;
        let mut accounts = BTreeMap::<i64, i128>::new();
        let mut used = HashSet::new();
        for row in &draft.amounts {
            if row.amount <= 0 || !used.insert(row.head_id) {
                return Err(Error::Validation(
                    "Enter a positive amount once per pay head".into(),
                ));
            }
            let (kind, ledger, liability): (String, i64, Option<i64>) = tx.query_row(
                "SELECT kind, ledger_id, liability_ledger_id FROM payroll_heads WHERE company_id = ?1 AND id = ?2",
                params![company, row.head_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).optional()?.ok_or_else(|| Error::Validation("Pay head not found in this company".into()))?;
            if ledger == payable || liability == Some(payable) {
                return Err(Error::Validation(
                    "A pay head ledger must differ from the employee's payable ledger".into(),
                ));
            }
            let amount = i128::from(row.amount);
            match PayHeadKind::parse(&kind)? {
                PayHeadKind::Earning => {
                    gross += amount;
                    *accounts.entry(ledger).or_default() += amount;
                }
                PayHeadKind::Deduction => {
                    deductions += amount;
                    *accounts.entry(ledger).or_default() -= amount;
                }
                PayHeadKind::EmployerContribution => {
                    employer += amount;
                    *accounts.entry(ledger).or_default() += amount;
                    let liability = liability.ok_or_else(|| {
                        Error::Validation("Contribution liability ledger is missing".into())
                    })?;
                    *accounts.entry(liability).or_default() -= amount;
                }
            }
        }
        if gross == 0 || deductions > gross {
            return Err(Error::Validation(
                "Payroll requires earnings and deductions cannot exceed them".into(),
            ));
        }
        let net = gross - deductions;
        *accounts.entry(payable).or_default() -= net;
        let postings = accounts
            .into_iter()
            .filter(|(_, amount)| *amount != 0)
            .map(|(ledger_id, amount)| {
                Ok(Posting {
                    ledger_id,
                    amount: supported_amount(amount)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let date = date.format("%Y-%m-%d").to_string();
        let from = from.format("%Y-%m-%d").to_string();
        let to = to.format("%Y-%m-%d").to_string();
        validate_voucher(&date, &postings)?;
        check_ledgers(&tx, company, &postings)?;
        crate::opening::check_no_control_postings(&tx, company, &postings)?;
        tx.execute(
            "INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![company, VoucherKind::Journal.as_str(), reference, date, draft.narration.trim()],
        )?;
        let voucher = tx.last_insert_rowid();
        for posting in postings {
            tx.execute(
                "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                params![voucher, posting.ledger_id, posting.amount],
            )?;
        }
        let inserted = tx.execute(
            "INSERT INTO payroll_accruals(company_id, employee_id, voucher_id, period_start, period_end,
                reference_key, gross, deductions, net, employer_contributions, payable_ledger_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT DO NOTHING",
            params![company, draft.employee_id, voucher, from, to, reference.to_lowercase(),
                supported_amount(gross)?, supported_amount(deductions)?, supported_amount(net)?, supported_amount(employer)?, payable],
        )?;
        if inserted != 1 {
            return Err(Error::Validation(
                "A payroll entry already uses this reference or employee and period".into(),
            ));
        }
        let accrual = tx.last_insert_rowid();
        for row in &draft.amounts {
            tx.execute(
                "INSERT INTO payroll_lines(accrual_id, head_id, amount) VALUES (?1, ?2, ?3)",
                params![accrual, row.head_id, row.amount],
            )?;
        }
        tx.commit()?;
        Ok(accrual)
    }

    pub fn payroll_register(
        &self,
        company: i64,
        start: Option<&str>,
        end: Option<&str>,
        employee: Option<i64>,
    ) -> Result<Vec<PayrollAccrual>> {
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(start), Some(end)) if start > end) {
            return Err(Error::Validation(
                "Payroll register start date cannot follow its end date".into(),
            ));
        }
        let mut stmt = self.conn.prepare(
            "SELECT a.id, e.id, e.code, e.name, e.department, v.id, v.date, a.period_start, a.period_end,
                    v.reference, v.narration, a.gross, a.deductions, a.net, a.employer_contributions
             FROM payroll_accruals a JOIN payroll_employees e ON e.id = a.employee_id
             JOIN vouchers v ON v.id = a.voucher_id
             WHERE a.company_id = ?1 AND (?2 IS NULL OR e.id = ?2)",
        )?;
        let mut rows = stmt.query(params![company, employee])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let date: String = row.get(6)?;
            let parsed = parse_date(&date)?;
            if start.is_some_and(|from| parsed < from) || end.is_some_and(|end| parsed > end) {
                continue;
            }
            let id = row.get(0)?;
            let net = row.get::<_, i64>(13)?;
            let paid = crate::payroll_payments::payment_state(&self.conn, company, id, end)?.paid;
            let outstanding = i128::from(net) - paid;
            if outstanding < 0 {
                return Err(Error::Validation(
                    "Payroll payments exceed the accrued amount".into(),
                ));
            }
            result.push((
                parsed,
                PayrollAccrual {
                    id,
                    employee_id: row.get(1)?,
                    code: row.get(2)?,
                    employee: row.get(3)?,
                    department: row.get(4)?,
                    voucher_id: row.get(5)?,
                    date,
                    period_start: row.get(7)?,
                    period_end: row.get(8)?,
                    reference: row.get(9)?,
                    narration: row.get(10)?,
                    gross: row.get(11)?,
                    deductions: row.get(12)?,
                    net,
                    paid,
                    outstanding,
                    employer_contributions: row.get(14)?,
                },
            ));
        }
        result.sort_by(|(a, first), (b, second)| b.cmp(a).then_with(|| second.id.cmp(&first.id)));
        Ok(result.into_iter().map(|(_, entry)| entry).collect())
    }

    pub fn payroll_lines(&self, company: i64, accrual: i64) -> Result<Vec<PayrollLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT h.name, h.kind, l.amount FROM payroll_lines l
             JOIN payroll_heads h ON h.id = l.head_id JOIN payroll_accruals a ON a.id = l.accrual_id
             WHERE a.company_id = ?1 AND a.id = ?2 ORDER BY l.id",
        )?;
        let mut rows = stmt.query(params![company, accrual])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            result.push(PayrollLine {
                head: row.get(0)?,
                kind: PayHeadKind::parse(&row.get::<_, String>(1)?)?,
                amount: row.get(2)?,
            });
        }
        Ok(result)
    }
}
