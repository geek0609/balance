//! Ledger defaults on stock-only documents are used when preparing an invoice.
use super::*;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS order_line_trade_defaults(
            order_line_id INTEGER PRIMARY KEY REFERENCES order_lines(id) ON DELETE CASCADE,
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id)
         );
         CREATE TABLE IF NOT EXISTS stock_note_trade_defaults(
            note_line_id INTEGER PRIMARY KEY REFERENCES stock_note_lines(id) ON DELETE CASCADE,
            ledger_id INTEGER NOT NULL REFERENCES ledgers(id)
         );",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct TradeDefault {
    name: String,
    existing_id: Option<i64>,
}

impl TradeDefault {
    pub(super) fn same_ledger(&self, other: &Self) -> bool {
        normalized(&self.name) == normalized(&other.name)
    }

    pub(super) fn resolve(
        &self,
        conn: &Connection,
        company: i64,
        party: i64,
        kind: InvoiceKind,
    ) -> Result<i64> {
        let id = find_ledger(conn, company, &self.name)?
            .ok_or_else(|| Error::Validation(format!("Ledger {} is missing", self.name)))?;
        let category = crate::inventory::ledger_category(conn, company, id)?;
        if self.existing_id.is_some_and(|expected| expected != id)
            || id == party
            || !compatible(kind, category)
        {
            return Err(Error::Validation(format!(
                "Trade ledger {} changed since preview",
                self.name
            )));
        }
        Ok(id)
    }
}

pub(super) fn compatible(kind: InvoiceKind, category: Category) -> bool {
    match kind {
        InvoiceKind::Sale => category == Category::Income,
        InvoiceKind::Purchase => matches!(category, Category::Expense | Category::Asset),
    }
}

pub(super) struct SourceAccounts<'a> {
    pub kind: InvoiceKind,
    pub party: &'a str,
    pub ledgers: &'a HashMap<String, (Option<i64>, Category)>,
    pub label: &'a str,
    pub strict: bool,
}

pub(super) struct VoucherDefaults {
    pub common: Option<TradeDefault>,
    pub requires_line_accounts: bool,
    pub valued_accounts: Vec<TradeDefault>,
}

impl SourceAccounts<'_> {
    fn account(&self, name: String) -> Option<TradeDefault> {
        let key = normalized(&name);
        let &(existing_id, category) = self.ledgers.get(&key)?;
        (key != normalized(self.party) && compatible(self.kind, category))
            .then_some(TradeDefault { name, existing_id })
    }

    pub(super) fn voucher(
        &self,
        node: Node<'_, '_>,
        issues: &mut Vec<String>,
    ) -> Result<VoucherDefaults> {
        let mut accounts = Vec::<TradeDefault>::new();
        let mut valued_accounts = Vec::new();
        let mut has_amount = false;
        for entry in node.children().filter(|entry| {
            entry.is_element()
                && meaningful(*entry)
                && matches!(
                    local_name(*entry).to_ascii_uppercase().as_str(),
                    "LEDGERENTRIES.LIST" | "ALLLEDGERENTRIES.LIST"
                )
        }) {
            invoice_list_fields(
                entry,
                &[
                    "LEDGERNAME",
                    "AMOUNT",
                    "ISDEEMEDPOSITIVE",
                    "ISPARTYLEDGER",
                    "ISLASTDEEMEDPOSITIVE",
                ],
                self.label,
                issues,
            );
            let amount = xml_item_amount(entry)?;
            has_amount |= amount != 0;
            let name = direct_text(entry, "LEDGERNAME").unwrap_or_default();
            if normalized(&name) == normalized(self.party) {
                continue;
            }
            if let Some(account) = self.account(name) {
                if self.strict && amount != 0 {
                    valued_accounts.push(account.clone());
                }
                if !accounts.iter().any(|saved| saved.same_ledger(&account)) {
                    accounts.push(account);
                }
            } else if amount != 0 && self.strict {
                return Err(Error::Validation(
                    "Stock note contains an amount for an unsupported ledger".into(),
                ));
            }
        }
        Ok(VoucherDefaults {
            requires_line_accounts: self.strict && (has_amount || accounts.len() > 1),
            common: (accounts.len() == 1).then(|| accounts.remove(0)),
            valued_accounts,
        })
    }

    pub(super) fn item(
        &self,
        node: Node<'_, '_>,
        fallback: Option<&TradeDefault>,
        issues: &mut Vec<String>,
    ) -> Result<Option<TradeDefault>> {
        let mut accounts = Vec::<TradeDefault>::new();
        let mut has_allocation = false;
        for entry in node.children().filter(|entry| {
            entry.is_element()
                && meaningful(*entry)
                && local_name(*entry).eq_ignore_ascii_case("ACCOUNTINGALLOCATIONS.LIST")
        }) {
            invoice_list_fields(
                entry,
                &[
                    "LEDGERNAME",
                    "AMOUNT",
                    "ISDEEMEDPOSITIVE",
                    "ISLASTDEEMEDPOSITIVE",
                ],
                self.label,
                issues,
            );
            let amount = xml_item_amount(entry)?;
            let name = direct_text(entry, "LEDGERNAME").unwrap_or_default();
            has_allocation |= !name.is_empty() || amount != 0;
            if let Some(account) = self.account(name.clone()) {
                if !accounts.iter().any(|saved| saved.same_ledger(&account)) {
                    accounts.push(account);
                }
            } else if self.strict && (!name.is_empty() || amount != 0) {
                return Err(Error::Validation(format!(
                    "Stock note item ledger {name} is unavailable or incompatible",
                )));
            }
        }
        match accounts.len() {
            1 => Ok(accounts.pop()),
            0 if !has_allocation => Ok(fallback.cloned()),
            n if self.strict && n > 1 => Err(Error::Validation(
                "Several trade ledgers on one stock allocation require separate item lines".into(),
            )),
            _ => Ok(None),
        }
    }
}
