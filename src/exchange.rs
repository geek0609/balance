//! Import of a limited accounting XML interchange format.
//! Unsupported or ambiguous data is reported before any database writes.
use crate::contacts::{insert_new_profile, LedgerProfile};
use crate::{
    check_ledgers, parse_amount, parse_date, validate_voucher, Category, Error, Posting, Result,
    Store, VoucherKind,
};
use chrono::NaiveDate;
use roxmltree::{Document, Node, ParsingOptions};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

const MAX_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Group {
    name: String,
    parent: String,
    category: Category,
}
#[derive(Debug, Clone)]
struct Account {
    name: String,
    group: String,
    category: Category,
    profile: LedgerProfile,
}
#[derive(Debug, Clone)]
struct Line {
    ledger: String,
    amount: i64,
}
#[derive(Debug, Clone)]
struct Transaction {
    kind: VoucherKind,
    reference: String,
    date: String,
    narration: String,
    lines: Vec<Line>,
}

/// An immutable preview. Only previews without issues can be applied.
#[derive(Debug, Clone)]
pub struct ImportPlan {
    company_id: i64,
    groups: Vec<Group>,
    accounts: Vec<Account>,
    transactions: Vec<Transaction>,
    issues: Vec<String>,
}
impl ImportPlan {
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
    pub fn ledger_count(&self) -> usize {
        self.accounts.len()
    }
    pub fn voucher_count(&self) -> usize {
        self.transactions.len()
    }
    pub fn company_id(&self) -> i64 {
        self.company_id
    }
    pub fn issues(&self) -> &[String] {
        &self.issues
    }
    pub fn can_apply(&self) -> bool {
        self.issues.is_empty()
            && (!self.groups.is_empty()
                || !self.accounts.is_empty()
                || !self.transactions.is_empty())
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ImportCounts {
    pub groups: usize,
    pub ledgers: usize,
    pub vouchers: usize,
}

fn normalized(value: &str) -> String {
    value.trim().to_lowercase()
}
fn local_name<'a>(node: Node<'_, 'a>) -> &'a str {
    node.tag_name().name()
}
fn direct_text(node: Node<'_, '_>, name: &str) -> Option<String> {
    node.children()
        .find(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case(name))
        .map(|n| {
            n.children()
                .filter(|c| c.is_text())
                .filter_map(|c| c.text())
                .collect::<String>()
                .trim()
                .to_owned()
        })
}
fn object_name(node: Node<'_, '_>) -> Option<String> {
    let child = direct_text(node, "NAME").filter(|v| !v.is_empty());
    let attr = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("NAME"))
        .map(|a| a.value().trim().to_owned())
        .filter(|v| !v.is_empty());
    match (child, attr) {
        (Some(a), Some(b)) if a != b => None,
        (Some(a), _) | (_, Some(a)) => Some(a),
        _ => None,
    }
}
fn meaningful(node: Node<'_, '_>) -> bool {
    node.descendants().any(|n| {
        n.text().is_some_and(|t| !t.trim().is_empty())
            || (n.is_element() && n.attributes().any(|a| !a.value().trim().is_empty()))
    })
}
fn check_fields(node: Node<'_, '_>, allowed: &[&str], label: &str, issues: &mut Vec<String>) {
    let mut singletons = HashSet::new();
    for child in node.children().filter(|n| n.is_element()) {
        let tag = local_name(child);
        if !allowed.iter().any(|a| tag.eq_ignore_ascii_case(a)) && meaningful(child) {
            issues.push(format!("{label}: field {tag} is not supported"));
        }
        if !tag.to_ascii_uppercase().ends_with(".LIST")
            && !singletons.insert(tag.to_ascii_uppercase())
        {
            issues.push(format!("{label}: field {tag} occurs more than once"));
        }
        if allowed.iter().any(|a| tag.eq_ignore_ascii_case(a))
            && !tag.to_ascii_uppercase().ends_with(".LIST")
        {
            if child.children().any(|c| c.is_element()) {
                issues.push(format!("{label}: nested field {tag} is not supported"));
            }
            for attribute in child.attributes() {
                if !attribute.name().eq_ignore_ascii_case("TYPE")
                    || !["String", "Date", "Number", "Logical", "Amount"]
                        .iter()
                        .any(|v| attribute.value().eq_ignore_ascii_case(v))
                {
                    issues.push(format!(
                        "{label}: attribute {} on {tag} is not supported",
                        attribute.name()
                    ));
                }
            }
        }
    }
    for attribute in node.attributes() {
        if !attribute.name().eq_ignore_ascii_case("NAME")
            && !attribute.name().eq_ignore_ascii_case("ACTION")
            && !(attribute.name().eq_ignore_ascii_case("TYPE")
                && ["String", "Date", "Number", "Logical", "Amount"]
                    .iter()
                    .any(|v| attribute.value().eq_ignore_ascii_case(v)))
            && !attribute.value().trim().is_empty()
        {
            issues.push(format!(
                "{label}: attribute {} is not supported",
                attribute.name()
            ));
        }
    }
    if let Some(action) = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("ACTION"))
    {
        if !action.value().eq_ignore_ascii_case("create") && !action.value().is_empty() {
            issues.push(format!("{label}: only new records can be imported"));
        }
    }
}
fn check_language_names(node: Node<'_, '_>, name: &str, label: &str, issues: &mut Vec<String>) {
    let lists: Vec<_> = node
        .children()
        .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("LANGUAGENAME.LIST"))
        .collect();
    if lists.len() > 1 {
        issues.push(format!(
            "{label}: multiple language name lists are not supported"
        ));
    }
    for list in lists {
        check_fields(list, &["NAME.LIST", "LANGUAGEID"], label, issues);
        let name_lists: Vec<_> = list
            .children()
            .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("NAME.LIST"))
            .collect();
        if name_lists.len() != 1 {
            issues.push(format!("{label}: name list is ambiguous"));
            continue;
        }
        let names = name_lists[0];
        for attr in names.attributes() {
            if !attr.name().eq_ignore_ascii_case("TYPE")
                || !attr.value().eq_ignore_ascii_case("String")
            {
                issues.push(format!("{label}: name list attribute is not supported"));
            }
        }
        check_fields(names, &["NAME"], label, issues);
        if direct_text(names, "NAME").as_deref() != Some(name) {
            issues.push(format!(
                "{label}: aliases or alternate names are not supported"
            ));
        }
        if direct_text(list, "LANGUAGEID").is_some_and(|id| !id.is_empty() && id != "1033") {
            issues.push(format!("{label}: alternate languages are not supported"));
        }
    }
}
fn profile_list_values(
    node: Node<'_, '_>,
    list_name: &str,
    item_name: &str,
    label: &str,
    issues: &mut Vec<String>,
) -> Vec<String> {
    let lists: Vec<_> = node
        .children()
        .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case(list_name))
        .collect();
    if lists.len() > 1 {
        issues.push(format!(
            "{label}: multiple {list_name} lists are not supported"
        ));
    }
    let mut values = Vec::new();
    for list in lists {
        for attr in list.attributes() {
            if !attr.name().eq_ignore_ascii_case("TYPE")
                || !attr.value().eq_ignore_ascii_case("String")
            {
                issues.push(format!(
                    "{label}: attribute {} on {list_name} is not supported",
                    attr.name()
                ));
            }
        }
        for child in list.children().filter(|n| n.is_element()) {
            if !local_name(child).eq_ignore_ascii_case(item_name) {
                if meaningful(child) {
                    issues.push(format!(
                        "{label}: field {} is not supported",
                        local_name(child)
                    ));
                }
                continue;
            }
            if child.children().any(|n| n.is_element()) || child.attributes().len() != 0 {
                issues.push(format!("{label}: nested {item_name} is not supported"));
                continue;
            }
            let text = child
                .children()
                .filter(|n| n.is_text())
                .filter_map(|n| n.text())
                .collect::<String>();
            values.push(text.trim().to_owned());
        }
    }
    values
}
fn decode(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Validation(
            "The XML file exceeds the 50 MiB import limit".into(),
        ));
    }
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if bytes.len() % 2 != 0 {
            return Err(Error::Validation("Invalid UTF-16 XML".into()));
        }
        let le = bytes[0] == 0xff;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|x| {
                if le {
                    u16::from_le_bytes([x[0], x[1]])
                } else {
                    u16::from_be_bytes([x[0], x[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| Error::Validation("Invalid UTF-16 XML".into()))
    } else {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            Error::Validation("XML must be UTF-8 or UTF-16 with a byte-order mark".into())
        })?;
        Ok(text.trim_start_matches('\u{feff}').to_owned())
    }
}
fn parse_kind(value: &str) -> Option<VoucherKind> {
    match normalized(value).as_str() {
        "journal" => Some(VoucherKind::Journal),
        "payment" => Some(VoucherKind::Payment),
        "receipt" => Some(VoucherKind::Receipt),
        "contra" | "transfer" => Some(VoucherKind::Transfer),
        "sales" => Some(VoucherKind::Sales),
        "purchase" => Some(VoucherKind::Purchase),
        "credit note" => Some(VoucherKind::CreditNote),
        "debit note" => Some(VoucherKind::DebitNote),
        _ => None,
    }
}
fn xml_date(value: &str) -> Result<String> {
    let parsed = if value.trim().len() == 8 && value.trim().bytes().all(|b| b.is_ascii_digit()) {
        NaiveDate::parse_from_str(value.trim(), "%Y%m%d")
            .map_err(|_| Error::Validation("Invalid transaction date".into()))?
    } else {
        parse_date(value)?
    };
    Ok(parsed.format("%Y-%m-%d").to_string())
}
fn xml_amount(value: &str) -> Result<i64> {
    let trimmed = value.trim();
    // The interchange convention represents a debit as a negative amount.
    if let Some(rest) = trimmed.strip_prefix('-') {
        parse_amount(rest)
    } else {
        parse_amount(trimmed)?
            .checked_neg()
            .ok_or_else(|| Error::Validation("Amount is too large".into()))
    }
}
fn find_group(conn: &Connection, company: i64, name: &str) -> Result<Option<(i64, Category)>> {
    let mut stmt =
        conn.prepare("SELECT id, name, category FROM account_groups WHERE company_id = ?1")?;
    let rows = stmt.query_map([company], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut found = None;
    for row in rows {
        let (id, candidate, category) = row?;
        if normalized(&candidate) == normalized(name) {
            if found.is_some() {
                return Err(Error::Validation(format!("Group {name} is ambiguous")));
            }
            found = Some((id, Category::parse(&category)?));
        }
    }
    Ok(found)
}
fn find_ledger(conn: &Connection, company: i64, name: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id, name FROM ledgers WHERE company_id = ?1")?;
    let rows = stmt.query_map([company], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut found = None;
    for row in rows {
        let (id, candidate) = row?;
        if normalized(&candidate) == normalized(name) {
            if found.is_some() {
                return Err(Error::Validation(format!("Ledger {name} is ambiguous")));
            }
            found = Some(id);
        }
    }
    Ok(found)
}

impl Store {
    pub fn preview_xml_import(&self, company_id: i64, bytes: &[u8]) -> Result<ImportPlan> {
        if !self.companies()?.iter().any(|c| c.id == company_id) {
            return Err(Error::Validation("Select an existing company".into()));
        }
        let text = decode(bytes)?;
        let doc = Document::parse_with_options(
            &text,
            ParsingOptions {
                nodes_limit: 1_000_000,
                ..ParsingOptions::default()
            },
        )
        .map_err(|e| Error::Validation(format!("Cannot read XML: {e}")))?;
        let mut plan = ImportPlan {
            company_id,
            groups: Vec::new(),
            accounts: Vec::new(),
            transactions: Vec::new(),
            issues: Vec::new(),
        };
        for node in doc.descendants().filter(|n| {
            n.is_element()
                && n.parent().is_some_and(|p| {
                    p.is_element()
                        && (local_name(p).to_ascii_uppercase().ends_with("MESSAGE")
                            || local_name(p).eq_ignore_ascii_case("COLLECTION"))
                })
        }) {
            if !["GROUP", "LEDGER", "VOUCHER"]
                .iter()
                .any(|name| local_name(node).eq_ignore_ascii_case(name))
                && meaningful(node)
            {
                plan.issues
                    .push(format!("{} records are not supported", local_name(node)));
            }
        }
        let existing_groups = self.account_groups(company_id)?;
        let mut known_groups = HashMap::<String, Category>::new();
        for group in &existing_groups {
            if known_groups
                .insert(normalized(&group.name), group.category)
                .is_some()
            {
                plan.issues
                    .push(format!("Group {} is ambiguous", group.name));
            }
        }
        let mut pending_groups = Vec::<(String, String)>::new();
        for node in doc
            .descendants()
            .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("GROUP"))
        {
            let Some(name) = object_name(node) else {
                plan.issues
                    .push("An account group has no unambiguous name".into());
                continue;
            };
            check_fields(
                node,
                &["NAME", "PARENT", "LANGUAGENAME.LIST"],
                &format!("Group {name}"),
                &mut plan.issues,
            );
            check_language_names(node, &name, &format!("Group {name}"), &mut plan.issues);
            let parent = direct_text(node, "PARENT").unwrap_or_default();
            if let Some(existing) = existing_groups
                .iter()
                .find(|g| normalized(&g.name) == normalized(&name))
            {
                let expected_parent = existing
                    .parent_id
                    .and_then(|id| existing_groups.iter().find(|g| g.id == id))
                    .map(|g| normalized(&g.name));
                if !parent.is_empty()
                    && expected_parent.as_deref() != Some(normalized(&parent).as_str())
                {
                    plan.issues.push(format!(
                        "Group {name}: existing parent differs from the imported parent"
                    ));
                }
                continue;
            }
            if parent.is_empty() {
                plan.issues.push(format!("Group {name}: parent is missing"));
                continue;
            }
            if pending_groups
                .iter()
                .any(|(n, _)| normalized(n) == normalized(&name))
            {
                plan.issues.push(format!("Group {name}: duplicate name"));
                continue;
            }
            pending_groups.push((name, parent));
        }
        while !pending_groups.is_empty() {
            let mut progress = false;
            let mut unresolved = Vec::new();
            for (name, parent) in pending_groups {
                if let Some(&category) = known_groups.get(&normalized(&parent)) {
                    known_groups.insert(normalized(&name), category);
                    plan.groups.push(Group {
                        name,
                        parent,
                        category,
                    });
                    progress = true;
                } else {
                    unresolved.push((name, parent));
                }
            }
            if !progress {
                for (name, parent) in unresolved {
                    plan.issues.push(format!(
                        "Group {name}: parent {parent} is missing or cyclic"
                    ));
                }
                break;
            }
            pending_groups = unresolved;
        }
        let mut known_ledgers = HashSet::<String>::new();
        for ledger in self.ledgers(company_id)? {
            if !known_ledgers.insert(normalized(&ledger.name)) {
                plan.issues
                    .push(format!("Ledger {} is ambiguous", ledger.name));
            }
        }
        for node in doc.descendants().filter(|n| {
            n.is_element()
                && local_name(*n).eq_ignore_ascii_case("LEDGER")
                && !n
                    .ancestors()
                    .any(|a| a.is_element() && local_name(a).eq_ignore_ascii_case("VOUCHER"))
        }) {
            let Some(name) = object_name(node) else {
                plan.issues.push("A ledger has no unambiguous name".into());
                continue;
            };
            check_fields(
                node,
                &[
                    "NAME",
                    "PARENT",
                    "OPENINGBALANCE",
                    "LANGUAGENAME.LIST",
                    "MAILINGNAME.LIST",
                    "ADDRESS.LIST",
                    "PINCODE",
                    "COUNTRYNAME",
                    "LEDSTATENAME",
                    "EMAIL",
                    "EMAILCC",
                    "LEDGERPHONE",
                    "LEDGERMOBILE",
                    "PARTYGSTIN",
                    "GSTREGISTRATIONTYPE",
                ],
                &format!("Ledger {name}"),
                &mut plan.issues,
            );
            check_language_names(node, &name, &format!("Ledger {name}"), &mut plan.issues);
            let label = format!("Ledger {name}");
            let mailing = profile_list_values(
                node,
                "MAILINGNAME.LIST",
                "MAILINGNAME",
                &label,
                &mut plan.issues,
            );
            if mailing.len() > 1 {
                plan.issues
                    .push(format!("{label}: multiple mailing names are not supported"));
            }
            let address =
                profile_list_values(node, "ADDRESS.LIST", "ADDRESS", &label, &mut plan.issues)
                    .join("\n");
            let profile = LedgerProfile {
                mailing_name: mailing.first().cloned().unwrap_or_default(),
                address,
                postal_code: direct_text(node, "PINCODE").unwrap_or_default(),
                country: direct_text(node, "COUNTRYNAME").unwrap_or_default(),
                region: direct_text(node, "LEDSTATENAME").unwrap_or_default(),
                email: direct_text(node, "EMAIL").unwrap_or_default(),
                email_cc: direct_text(node, "EMAILCC").unwrap_or_default(),
                phone: direct_text(node, "LEDGERPHONE").unwrap_or_default(),
                mobile: direct_text(node, "LEDGERMOBILE").unwrap_or_default(),
                tax_id: direct_text(node, "PARTYGSTIN").unwrap_or_default(),
                tax_registration_type: direct_text(node, "GSTREGISTRATIONTYPE").unwrap_or_default(),
            };
            if let Some(opening) = direct_text(node, "OPENINGBALANCE") {
                if !opening.is_empty() && !matches!(xml_amount(&opening), Ok(0)) {
                    plan.issues
                        .push(format!("Ledger {name}: opening balances are not supported"));
                }
            }
            let group = direct_text(node, "PARENT").unwrap_or_default();
            let category = match known_groups.get(&normalized(&group)) {
                Some(category) => *category,
                None => {
                    plan.issues
                        .push(format!("Ledger {name}: group {group} is not available"));
                    continue;
                }
            };
            if !known_ledgers.insert(normalized(&name)) {
                plan.issues.push(format!("Ledger {name}: duplicate name"));
            }
            plan.accounts.push(Account {
                name,
                group,
                category,
                profile,
            });
        }
        let mut references: HashSet<(String, String, String)> = self
            .vouchers(company_id)?
            .iter()
            .filter(|v| !v.reference.is_empty())
            .map(|v| {
                (
                    normalized(v.kind.as_str()),
                    v.date.clone(),
                    normalized(&v.reference),
                )
            })
            .collect();
        for node in doc
            .descendants()
            .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("VOUCHER"))
        {
            let label = format!(
                "Voucher {}",
                direct_text(node, "VOUCHERNUMBER").unwrap_or_default()
            );
            check_fields(
                node,
                &[
                    "DATE",
                    "VOUCHERTYPENAME",
                    "VOUCHERNUMBER",
                    "NARRATION",
                    "PERSISTEDVIEW",
                    "ISINVOICE",
                    "LEDGERENTRIES.LIST",
                    "ALLLEDGERENTRIES.LIST",
                ],
                &label,
                &mut plan.issues,
            );
            if direct_text(node, "ISINVOICE").is_some_and(|v| !v.eq_ignore_ascii_case("no")) {
                plan.issues
                    .push(format!("{label}: invoices and inventory are not supported"));
            }
            if direct_text(node, "PERSISTEDVIEW").is_some_and(|v| {
                !v.is_empty() && !v.eq_ignore_ascii_case("Accounting Voucher View")
            }) {
                plan.issues
                    .push(format!("{label}: unsupported voucher view"));
            }
            let kind = match direct_text(node, "VOUCHERTYPENAME").and_then(|v| parse_kind(&v)) {
                Some(v) => v,
                None => {
                    plan.issues
                        .push(format!("{label}: voucher type is not supported"));
                    continue;
                }
            };
            let date = match direct_text(node, "DATE").and_then(|v| xml_date(&v).ok()) {
                Some(v) => v,
                None => {
                    plan.issues
                        .push(format!("{label}: date is missing or invalid"));
                    continue;
                }
            };
            let reference = direct_text(node, "VOUCHERNUMBER").unwrap_or_default();
            if reference.is_empty() {
                plan.issues
                    .push(format!("{label}: a reference number is required"));
            } else if !references.insert((
                normalized(kind.as_str()),
                date.clone(),
                normalized(&reference),
            )) {
                plan.issues.push(format!(
                    "{label}: a voucher with the same type, date and number already exists"
                ));
            }
            let narration = direct_text(node, "NARRATION").unwrap_or_default();
            let mut lines = Vec::new();
            let has_entries = node.children().any(|n| {
                n.is_element() && local_name(n).eq_ignore_ascii_case("LEDGERENTRIES.LIST")
            });
            let has_all_entries = node.children().any(|n| {
                n.is_element() && local_name(n).eq_ignore_ascii_case("ALLLEDGERENTRIES.LIST")
            });
            if has_entries && has_all_entries {
                plan.issues.push(format!(
                    "{label}: multiple ledger entry representations are ambiguous"
                ));
            }
            for entry in node.children().filter(|n| {
                n.is_element()
                    && (local_name(*n).eq_ignore_ascii_case("LEDGERENTRIES.LIST")
                        || local_name(*n).eq_ignore_ascii_case("ALLLEDGERENTRIES.LIST"))
            }) {
                check_fields(
                    entry,
                    &[
                        "LEDGERNAME",
                        "AMOUNT",
                        "ISDEEMEDPOSITIVE",
                        "ISPARTYLEDGER",
                        "ISLASTDEEMEDPOSITIVE",
                    ],
                    &label,
                    &mut plan.issues,
                );
                if direct_text(entry, "ISPARTYLEDGER")
                    .is_some_and(|v| v.eq_ignore_ascii_case("yes"))
                {
                    plan.issues.push(format!(
                        "{label}: party-specific information is not supported"
                    ));
                }
                let ledger = direct_text(entry, "LEDGERNAME").unwrap_or_default();
                if !known_ledgers.contains(&normalized(&ledger)) {
                    plan.issues
                        .push(format!("{label}: ledger {ledger} is not available"));
                }
                let amount = match direct_text(entry, "AMOUNT").and_then(|v| xml_amount(&v).ok()) {
                    Some(v) if v != 0 => v,
                    _ => {
                        plan.issues
                            .push(format!("{label}: ledger amount is invalid"));
                        continue;
                    }
                };
                if let Some(side) = direct_text(entry, "ISDEEMEDPOSITIVE") {
                    if (side.eq_ignore_ascii_case("yes") && amount < 0)
                        || (side.eq_ignore_ascii_case("no") && amount > 0)
                        || (!side.eq_ignore_ascii_case("yes") && !side.eq_ignore_ascii_case("no"))
                    {
                        plan.issues
                            .push(format!("{label}: debit or credit direction is ambiguous"));
                    }
                }
                if let Some(side) = direct_text(entry, "ISLASTDEEMEDPOSITIVE") {
                    if (side.eq_ignore_ascii_case("yes") && amount < 0)
                        || (side.eq_ignore_ascii_case("no") && amount > 0)
                        || (!side.eq_ignore_ascii_case("yes") && !side.eq_ignore_ascii_case("no"))
                    {
                        plan.issues
                            .push(format!("{label}: debit or credit direction is ambiguous"));
                    }
                }
                lines.push(Line { ledger, amount });
            }
            if lines.len() < 2
                || lines.iter().map(|l| i128::from(l.amount)).sum::<i128>() != 0
                || lines
                    .iter()
                    .all(|l| normalized(&l.ledger) == normalized(&lines[0].ledger))
            {
                plan.issues.push(format!(
                    "{label}: postings must balance across at least two ledgers"
                ));
            }
            plan.transactions.push(Transaction {
                kind,
                reference,
                date,
                narration,
                lines,
            });
        }
        for node in doc.descendants().filter(|n| {
            n.is_element()
                && ["STOCKITEM", "STOCKGROUP", "UNIT", "GODOWN", "VOUCHERTYPE"]
                    .iter()
                    .any(|tag| local_name(*n).eq_ignore_ascii_case(tag))
        }) {
            plan.issues
                .push(format!("{} records are not supported", local_name(node)));
        }
        if plan.groups.is_empty()
            && plan.accounts.is_empty()
            && plan.transactions.is_empty()
            && plan.issues.is_empty()
        {
            plan.issues
                .push("No supported accounting records were found".into());
        }
        Ok(plan)
    }
    /// Apply the exact preview atomically. No records are written on any error.
    pub fn apply_xml_import(&mut self, plan: &ImportPlan) -> Result<ImportCounts> {
        if !plan.can_apply() {
            return Err(Error::Validation(
                "Resolve all import issues before importing".into(),
            ));
        }
        let tx = self.conn.transaction()?;
        for group in &plan.groups {
            if find_group(&tx, plan.company_id, &group.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Group {} already exists",
                    group.name
                )));
            }
            let (parent, parent_category) = find_group(&tx, plan.company_id, &group.parent)?
                .ok_or_else(|| Error::Validation(format!("Group {} is missing", group.parent)))?;
            if parent_category != group.category {
                return Err(Error::Validation(format!(
                    "Group {} changed since preview",
                    group.parent
                )));
            }
            tx.execute("INSERT INTO account_groups(company_id, name, category, parent_id) VALUES (?1, ?2, ?3, ?4)", params![plan.company_id, group.name, group.category.as_str(), parent])?;
        }
        for account in &plan.accounts {
            if find_ledger(&tx, plan.company_id, &account.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Ledger {} already exists",
                    account.name
                )));
            }
            let (group_id, category) = find_group(&tx, plan.company_id, &account.group)?
                .ok_or_else(|| Error::Validation(format!("Group {} is missing", account.group)))?;
            if category != account.category {
                return Err(Error::Validation(format!(
                    "Group {} changed since preview",
                    account.group
                )));
            }
            tx.execute(
                "INSERT INTO ledgers(company_id, name, category, group_id) VALUES (?1, ?2, ?3, ?4)",
                params![plan.company_id, account.name, category.as_str(), group_id],
            )?;
            insert_new_profile(
                &tx,
                plan.company_id,
                tx.last_insert_rowid(),
                &account.profile,
            )?;
        }
        for voucher in &plan.transactions {
            let mut stmt =
                tx.prepare("SELECT kind, date, reference FROM vouchers WHERE company_id = ?1")?;
            let rows = stmt.query_map([plan.company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut duplicate = false;
            for row in rows {
                let (kind, date, reference) = row?;
                if normalized(&kind) == normalized(voucher.kind.as_str())
                    && xml_date(&date)? == voucher.date
                    && normalized(&reference) == normalized(&voucher.reference)
                {
                    duplicate = true;
                    break;
                }
            }
            drop(stmt);
            if duplicate {
                return Err(Error::Validation(format!(
                    "Voucher {} already exists",
                    voucher.reference
                )));
            }
            let mut postings = Vec::new();
            for line in &voucher.lines {
                let ledger_id =
                    find_ledger(&tx, plan.company_id, &line.ledger)?.ok_or_else(|| {
                        Error::Validation(format!("Ledger {} is missing", line.ledger))
                    })?;
                postings.push(Posting {
                    ledger_id,
                    amount: line.amount,
                });
            }
            validate_voucher(&voucher.date, &postings)?;
            check_ledgers(&tx, plan.company_id, &postings)?;
            tx.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![plan.company_id, voucher.kind.as_str(), voucher.reference, voucher.date, voucher.narration])?;
            let id = tx.last_insert_rowid();
            for posting in postings {
                tx.execute(
                    "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                    params![id, posting.ledger_id, posting.amount],
                )?;
            }
        }
        tx.commit()?;
        Ok(ImportCounts {
            groups: plan.groups.len(),
            ledgers: plan.accounts.len(),
            vouchers: plan.transactions.len(),
        })
    }
}
