//! Import of a limited accounting XML interchange format.
//! Unsupported or ambiguous data is reported before any database writes.
use crate::contacts::{insert_new_profile, LedgerProfile};
use crate::inventory::{
    check_quantity_precision, create_stock_batch_on_connection, create_stock_item_on_connection,
    find_stock_batch, insert_invoice_on_connection, line_amount, parse_quantity, InvoiceAdjustment,
    InvoiceDraft, InvoiceKind, InvoiceLine,
};
use crate::{
    check_ledgers, parse_amount, parse_date, validate_voucher, Category, Error, Posting, Result,
    Store, VoucherKind,
};
use chrono::NaiveDate;
use roxmltree::{Document, Node, ParsingOptions};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

const MAX_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Group {
    name: String,
    parent: String,
    category: Category,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct Account {
    name: String,
    group: String,
    category: Category,
    profile: LedgerProfile,
    opening: Option<i64>,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct Unit {
    symbol: String,
    formal_name: String,
    decimal_places: u8,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct StockGroupImport {
    name: String,
    parent: String,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct StockLocationImport {
    name: String,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct Item {
    name: String,
    unit: String,
    unit_precision: u8,
    group: String,
    description: String,
    openings: Vec<StockOpening>,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct StockOpening {
    location: String,
    existing_location_id: Option<i64>,
    batch: Option<String>,
    manufactured_on: Option<String>,
    quantity: i64,
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
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct ImportedInvoiceLine {
    item: String,
    existing_item_id: Option<i64>,
    unit: String,
    precision: u8,
    location: String,
    existing_location_id: Option<i64>,
    batch: Option<String>,
    existing_batch_id: Option<i64>,
    quantity: i64,
    unit_price: i64,
}
#[derive(Debug, Clone)]
struct ImportedInvoiceAdjustment {
    ledger: String,
    existing_ledger_id: Option<i64>,
    amount: i64,
}
#[derive(Debug, Clone)]
struct ImportedInvoice {
    kind: InvoiceKind,
    date: String,
    credit_days: Option<u32>,
    reference: String,
    narration: String,
    party: String,
    existing_party_id: Option<i64>,
    trade: String,
    existing_trade_id: Option<i64>,
    lines: Vec<ImportedInvoiceLine>,
    adjustments: Vec<ImportedInvoiceAdjustment>,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SourceMetadata {
    guid: String,
    remote_id: String,
    master_id: String,
    alteration_id: String,
}
impl SourceMetadata {
    fn is_empty(&self) -> bool {
        self.guid.is_empty()
            && self.remote_id.is_empty()
            && self.master_id.is_empty()
            && self.alteration_id.is_empty()
    }
    fn identities(&self) -> [(&'static str, &str); 3] {
        [
            ("guid", &self.guid),
            ("remote", &self.remote_id),
            ("master", &self.master_id),
        ]
    }
}

/// An immutable preview. Only previews without issues can be applied.
#[derive(Debug, Clone)]
pub struct ImportPlan {
    company_id: i64,
    opening_date: Option<String>,
    existing_group_metadata: Vec<(i64, SourceMetadata)>,
    existing_unit_metadata: Vec<(i64, SourceMetadata)>,
    existing_stock_group_metadata: Vec<(i64, SourceMetadata)>,
    existing_location_metadata: Vec<(i64, SourceMetadata)>,
    groups: Vec<Group>,
    accounts: Vec<Account>,
    units: Vec<Unit>,
    stock_groups: Vec<StockGroupImport>,
    locations: Vec<StockLocationImport>,
    items: Vec<Item>,
    transactions: Vec<Transaction>,
    invoices: Vec<ImportedInvoice>,
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
    pub fn invoice_count(&self) -> usize {
        self.invoices.len()
    }
    pub fn opening_count(&self) -> usize {
        self.accounts.iter().filter(|a| a.opening.is_some()).count()
    }
    pub fn unit_count(&self) -> usize {
        self.units.len()
    }
    pub fn stock_group_count(&self) -> usize {
        self.stock_groups.len()
    }
    pub fn item_count(&self) -> usize {
        self.items.len()
    }
    pub fn location_count(&self) -> usize {
        self.locations.len()
    }
    pub fn stock_opening_count(&self) -> usize {
        self.items
            .iter()
            .flat_map(|i| &i.openings)
            .filter(|opening| opening.quantity != 0)
            .count()
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
                || !self.existing_group_metadata.is_empty()
                || !self.existing_unit_metadata.is_empty()
                || !self.existing_stock_group_metadata.is_empty()
                || !self.existing_location_metadata.is_empty()
                || !self.units.is_empty()
                || !self.stock_groups.is_empty()
                || !self.locations.is_empty()
                || !self.items.is_empty()
                || !self.accounts.is_empty()
                || !self.transactions.is_empty()
                || !self.invoices.is_empty())
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ImportCounts {
    pub groups: usize,
    pub ledgers: usize,
    pub vouchers: usize,
    pub invoices: usize,
    pub units: usize,
    pub stock_groups: usize,
    pub locations: usize,
    pub items: usize,
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
    let record = [
        "GROUP",
        "LEDGER",
        "VOUCHER",
        "UNIT",
        "STOCKGROUP",
        "STOCKITEM",
        "GODOWN",
    ]
    .iter()
    .any(|tag| local_name(node).eq_ignore_ascii_case(tag));
    for child in node.children().filter(|n| n.is_element()) {
        let tag = local_name(child);
        let metadata = record
            && ["GUID", "REMOTEID", "MASTERID", "ALTERID"]
                .iter()
                .any(|a| tag.eq_ignore_ascii_case(a));
        if !allowed.iter().any(|a| tag.eq_ignore_ascii_case(a)) && !metadata && meaningful(child) {
            issues.push(format!("{label}: field {tag} is not supported"));
        }
        if !tag.to_ascii_uppercase().ends_with(".LIST")
            && !singletons.insert(tag.to_ascii_uppercase())
        {
            issues.push(format!("{label}: field {tag} occurs more than once"));
        }
        if (allowed.iter().any(|a| tag.eq_ignore_ascii_case(a)) || metadata)
            && !tag.to_ascii_uppercase().ends_with(".LIST")
        {
            if child.children().any(|c| c.is_element()) {
                issues.push(format!("{label}: nested field {tag} is not supported"));
            }
            for attribute in child.attributes() {
                let typed_rate = ["RATE", "OPENINGRATE"]
                    .iter()
                    .any(|v| tag.eq_ignore_ascii_case(v))
                    && attribute.value().eq_ignore_ascii_case("Rate");
                let typed_quantity = ["OPENINGBALANCE", "ACTUALQTY", "BILLEDQTY"]
                    .iter()
                    .any(|v| tag.eq_ignore_ascii_case(v))
                    && attribute.value().eq_ignore_ascii_case("Quantity");
                if !attribute.name().eq_ignore_ascii_case("TYPE")
                    || !(typed_rate
                        || typed_quantity
                        || ["String", "Date", "Number", "Logical", "Amount"]
                            .iter()
                            .any(|v| attribute.value().eq_ignore_ascii_case(v)))
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
            && !(record
                && ["GUID", "REMOTEID", "MASTERID", "ALTERID"]
                    .iter()
                    .any(|a| attribute.name().eq_ignore_ascii_case(a)))
            && !(local_name(node).eq_ignore_ascii_case("VOUCHER")
                && ["DATE", "VCHTYPE"]
                    .iter()
                    .any(|a| attribute.name().eq_ignore_ascii_case(a)))
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
fn source_metadata(node: Node<'_, '_>, label: &str, issues: &mut Vec<String>) -> SourceMetadata {
    let value = |name: &str, issues: &mut Vec<String>| {
        let child = direct_text(node, name).unwrap_or_default();
        let attribute = node
            .attributes()
            .find(|a| a.name().eq_ignore_ascii_case(name))
            .map(|a| a.value().trim().to_owned())
            .unwrap_or_default();
        if !child.is_empty() && !attribute.is_empty() && child != attribute {
            issues.push(format!("{label}: conflicting {name} values"));
        }
        if child.is_empty() {
            attribute
        } else {
            child
        }
    };
    SourceMetadata {
        guid: value("GUID", issues),
        remote_id: value("REMOTEID", issues),
        master_id: value("MASTERID", issues),
        alteration_id: value("ALTERID", issues),
    }
}
fn register_metadata(
    kind: &str,
    label: &str,
    meta: &SourceMetadata,
    seen: &mut HashSet<(String, String, String)>,
    issues: &mut Vec<String>,
) {
    for (key, value) in meta.identities() {
        if !value.is_empty() && !seen.insert((kind.to_owned(), key.to_owned(), normalized(value))) {
            issues.push(format!("{label}: imported identifier {key} already exists"));
        }
    }
}
fn save_metadata(
    conn: &Connection,
    company: i64,
    kind: &str,
    local_id: i64,
    meta: &SourceMetadata,
) -> Result<()> {
    if meta.is_empty() {
        return Ok(());
    }
    let existing: Option<(String, String, String, String)> = conn.query_row("SELECT guid, remote_id, master_id, alteration_id FROM imported_record_metadata WHERE company_id = ?1 AND kind = ?2 AND local_id = ?3", params![company, kind, local_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
    if let Some((guid, remote_id, master_id, alteration_id)) = existing {
        if *meta
            == (SourceMetadata {
                guid,
                remote_id,
                master_id,
                alteration_id,
            })
        {
            return Ok(());
        }
        return Err(Error::Validation(
            "Imported identifiers changed since preview".into(),
        ));
    }
    let mut stmt = conn.prepare("SELECT guid, remote_id, master_id FROM imported_record_metadata WHERE company_id = ?1 AND kind = ?2")?;
    let rows = stmt.query_map(params![company, kind], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (guid, remote, master) = row?;
        if (!meta.guid.is_empty() && normalized(&meta.guid) == normalized(&guid))
            || (!meta.remote_id.is_empty() && normalized(&meta.remote_id) == normalized(&remote))
            || (!meta.master_id.is_empty() && normalized(&meta.master_id) == normalized(&master))
        {
            return Err(Error::Validation(
                "An imported identifier already exists".into(),
            ));
        }
    }
    drop(stmt);
    conn.execute("INSERT INTO imported_record_metadata(company_id, kind, local_id, guid, remote_id, master_id, alteration_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![company, kind, local_id, meta.guid, meta.remote_id, meta.master_id, meta.alteration_id])?;
    Ok(())
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
fn xml_stock_quantity(value: &str, unit: &str) -> Result<i64> {
    let mut parts = value.split_whitespace();
    let number = parts
        .next()
        .ok_or_else(|| Error::Validation("Missing stock quantity".into()))?;
    let suffix = parts.collect::<Vec<_>>().join(" ");
    if !suffix.is_empty() && normalized(&suffix) != normalized(unit) {
        return Err(Error::Validation(
            "Stock quantity uses a different unit".into(),
        ));
    }
    if let Some(rest) = number.strip_prefix('-') {
        parse_quantity(rest)?
            .checked_neg()
            .ok_or_else(|| Error::Validation("Quantity is too large".into()))
    } else {
        parse_quantity(number)
    }
}
fn unvalued_opening_rate(value: &str, unit: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let (number, suffix) = value
        .split_once('/')
        .map_or((value, None), |(n, u)| (n, Some(u)));
    suffix.is_none_or(|suffix| normalized(suffix) == normalized(unit))
        && matches!(parse_amount(number), Ok(0))
}
fn invoice_field(node: Node<'_, '_>, field: &str) -> Result<String> {
    direct_text(node, field)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Validation(format!("missing {field}")))
}
fn invoice_side(node: Node<'_, '_>, field: &str, debit: bool) -> Result<()> {
    if let Some(value) = direct_text(node, field).filter(|v| !v.is_empty()) {
        if !value.eq_ignore_ascii_case(if debit { "yes" } else { "no" }) {
            return Err(Error::Validation(format!(
                "{field} conflicts with the amount"
            )));
        }
    }
    Ok(())
}
fn invoice_list_fields(
    node: Node<'_, '_>,
    allowed: &[&str],
    label: &str,
    issues: &mut Vec<String>,
) {
    check_fields(node, allowed, label, issues);
    if node.attributes().any(|a| !a.value().trim().is_empty()) {
        issues.push(format!(
            "{label}: attributes on {} are not supported",
            local_name(node)
        ));
    }
}
/// Parse only invoices whose party, item accounting, amounts and bill reference
/// can all be represented without dropping information.
fn parse_imported_invoice(
    node: Node<'_, '_>,
    label: &str,
    metadata: SourceMetadata,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    items: &HashMap<String, (Option<i64>, String, u8)>,
    locations: &HashMap<String, Option<i64>>,
    batches: &HashMap<(String, String), i64>,
    issues: &mut Vec<String>,
) -> Result<ImportedInvoice> {
    check_fields(
        node,
        &[
            "DATE",
            "VOUCHERTYPENAME",
            "VOUCHERNUMBER",
            "NARRATION",
            "PERSISTEDVIEW",
            "OBJVIEW",
            "ISINVOICE",
            "LEDGERENTRIES.LIST",
            "ALLLEDGERENTRIES.LIST",
            "ALLINVENTORYENTRIES.LIST",
        ],
        label,
        issues,
    );
    if node
        .attributes()
        .any(|a| a.name().eq_ignore_ascii_case("NAME") && !a.value().trim().is_empty())
    {
        issues.push(format!("{label}: voucher name attribute is not supported"));
    }
    for field in ["PERSISTEDVIEW", "OBJVIEW"] {
        if direct_text(node, field)
            .is_some_and(|v| !v.is_empty() && !v.eq_ignore_ascii_case("Invoice Voucher View"))
        {
            return Err(Error::Validation(format!("unsupported {field}")));
        }
    }
    let voucher_kind = parse_kind(&invoice_field(node, "VOUCHERTYPENAME")?)
        .ok_or_else(|| Error::Validation("unsupported invoice type".into()))?;
    let kind = match voucher_kind {
        VoucherKind::Sales => InvoiceKind::Sale,
        VoucherKind::Purchase => InvoiceKind::Purchase,
        _ => {
            return Err(Error::Validation(
                "only sales and purchase item invoices are supported".into(),
            ))
        }
    };
    let date = xml_date(&invoice_field(node, "DATE")?)?;
    if let Some(attr) = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("DATE"))
    {
        if xml_date(attr.value())? != date {
            return Err(Error::Validation("conflicting dates".into()));
        }
    }
    if let Some(attr) = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("VCHTYPE"))
    {
        if parse_kind(attr.value()) != Some(voucher_kind) {
            return Err(Error::Validation("conflicting invoice types".into()));
        }
    }
    let reference = invoice_field(node, "VOUCHERNUMBER")?;
    let entries: Vec<_> = node
        .children()
        .filter(|n| {
            n.is_element()
                && ["LEDGERENTRIES.LIST", "ALLLEDGERENTRIES.LIST"]
                    .iter()
                    .any(|tag| local_name(*n).eq_ignore_ascii_case(tag))
        })
        .collect();
    if entries
        .iter()
        .any(|n| local_name(*n).eq_ignore_ascii_case("LEDGERENTRIES.LIST"))
        && entries
            .iter()
            .any(|n| local_name(*n).eq_ignore_ascii_case("ALLLEDGERENTRIES.LIST"))
    {
        return Err(Error::Validation(
            "multiple ledger entry representations".into(),
        ));
    }
    let mut party: Option<(String, i64)> = None;
    let mut credit_days = None;
    let mut adjustments = Vec::new();
    let party_debit = kind == InvoiceKind::Sale;
    for entry in entries {
        invoice_list_fields(
            entry,
            &[
                "LEDGERNAME",
                "AMOUNT",
                "ISDEEMEDPOSITIVE",
                "ISLASTDEEMEDPOSITIVE",
                "ISPARTYLEDGER",
                "BILLALLOCATIONS.LIST",
            ],
            label,
            issues,
        );
        let ledger = invoice_field(entry, "LEDGERNAME")?;
        let (existing_ledger_id, category) = ledgers
            .get(&normalized(&ledger))
            .ok_or_else(|| Error::Validation(format!("ledger {ledger} is unavailable")))?;
        let amount = xml_amount(&invoice_field(entry, "AMOUNT")?)?;
        if amount == 0 {
            return Err(Error::Validation("zero ledger amount".into()));
        }
        invoice_side(entry, "ISDEEMEDPOSITIVE", amount > 0)?;
        invoice_side(entry, "ISLASTDEEMEDPOSITIVE", amount > 0)?;
        let is_party = match direct_text(entry, "ISPARTYLEDGER").as_deref() {
            Some(v) if v.eq_ignore_ascii_case("yes") => true,
            Some(v) if v.eq_ignore_ascii_case("no") || v.is_empty() => false,
            None => false,
            _ => return Err(Error::Validation("invalid party flag".into())),
        };
        let bills: Vec<_> = entry
            .children()
            .filter(|n| {
                n.is_element()
                    && local_name(*n).eq_ignore_ascii_case("BILLALLOCATIONS.LIST")
                    && meaningful(*n)
            })
            .collect();
        if is_party {
            if !matches!(category, Category::Asset | Category::Liability) {
                return Err(Error::Validation(
                    "party ledger must be an asset or liability".into(),
                ));
            }
            if party.is_some() || (amount > 0) != party_debit || bills.len() != 1 {
                return Err(Error::Validation(
                    "invoice needs one party and one new bill reference with the correct direction"
                        .into(),
                ));
            }
            let bill = bills[0];
            invoice_list_fields(
                bill,
                &["NAME", "BILLTYPE", "AMOUNT", "BILLCREDITPERIOD"],
                label,
                issues,
            );
            if invoice_field(bill, "NAME")? != reference
                || !invoice_field(bill, "BILLTYPE")?.eq_ignore_ascii_case("New Ref")
                || xml_amount(&invoice_field(bill, "AMOUNT")?)? != amount
            {
                return Err(Error::Validation(
                    "bill reference does not match the invoice".into(),
                ));
            }
            if let Some(term) = direct_text(bill, "BILLCREDITPERIOD").filter(|v| !v.is_empty()) {
                let parts: Vec<_> = term.split_whitespace().collect();
                if parts.len() != 2
                    || !parts[0].bytes().all(|b| b.is_ascii_digit())
                    || !(parts[1].eq_ignore_ascii_case("day")
                        || parts[1].eq_ignore_ascii_case("days"))
                {
                    return Err(Error::Validation(
                        "credit period must be a whole number of days".into(),
                    ));
                }
                let days = parts[0]
                    .parse::<u32>()
                    .map_err(|_| Error::Validation("credit period is too large".into()))?;
                parse_date(&date)?
                    .checked_add_days(chrono::Days::new(u64::from(days)))
                    .ok_or_else(|| {
                        Error::Validation("credit period produces an invalid due date".into())
                    })?;
                credit_days = Some(days);
            }
            party = Some((ledger, amount));
        } else {
            if !bills.is_empty() {
                return Err(Error::Validation(
                    "additional ledger bill allocations are not supported".into(),
                ));
            }
            adjustments.push(ImportedInvoiceAdjustment {
                ledger,
                existing_ledger_id: *existing_ledger_id,
                amount: if party_debit { -amount } else { amount },
            });
        }
    }
    let (party_name, party_amount) =
        party.ok_or_else(|| Error::Validation("party ledger is missing".into()))?;
    let mut lines = Vec::new();
    let mut trade: Option<String> = None;
    let mut subtotal = 0_i128;
    for item_node in node.children().filter(|n| {
        n.is_element() && local_name(*n).eq_ignore_ascii_case("ALLINVENTORYENTRIES.LIST")
    }) {
        invoice_list_fields(
            item_node,
            &[
                "STOCKITEMNAME",
                "ISDEEMEDPOSITIVE",
                "RATE",
                "AMOUNT",
                "ACTUALQTY",
                "BILLEDQTY",
                "ACCOUNTINGALLOCATIONS.LIST",
                "BATCHALLOCATIONS.LIST",
            ],
            label,
            issues,
        );
        let name = invoice_field(item_node, "STOCKITEMNAME")?;
        let (existing_item_id, unit, precision) = items
            .get(&normalized(&name))
            .ok_or_else(|| Error::Validation(format!("stock item {name} is unavailable")))?;
        let actual = xml_stock_quantity(&invoice_field(item_node, "ACTUALQTY")?, unit)?;
        let billed = xml_stock_quantity(&invoice_field(item_node, "BILLEDQTY")?, unit)?;
        if actual != billed || actual == 0 || (actual < 0 && kind == InvoiceKind::Purchase) {
            return Err(Error::Validation(
                "actual and billed quantities must match the supported stock direction".into(),
            ));
        }
        let quantity = actual
            .checked_abs()
            .ok_or_else(|| Error::Validation("quantity is too large".into()))?;
        if quantity % 10_i64.pow(u32::from(3 - *precision)) != 0 {
            return Err(Error::Validation("quantity exceeds unit precision".into()));
        }
        let rate = invoice_field(item_node, "RATE")?;
        let (price, rate_unit) = rate
            .split_once('/')
            .ok_or_else(|| Error::Validation("rate must specify the base unit".into()))?;
        if normalized(rate_unit) != normalized(unit) {
            return Err(Error::Validation("rate uses another unit".into()));
        }
        let unit_price = parse_amount(price)?;
        let amount = line_amount(quantity, unit_price)?;
        if amount == 0 {
            return Err(Error::Validation(
                "zero-value item lines are not supported".into(),
            ));
        }
        let source_amount = xml_amount(&invoice_field(item_node, "AMOUNT")?)?;
        let expected = if party_debit { -amount } else { amount };
        if source_amount != expected {
            return Err(Error::Validation(
                "item amount differs from quantity and rate".into(),
            ));
        }
        invoice_side(item_node, "ISDEEMEDPOSITIVE", expected > 0)?;
        let allocations: Vec<_> = item_node
            .children()
            .filter(|n| {
                n.is_element() && local_name(*n).eq_ignore_ascii_case("ACCOUNTINGALLOCATIONS.LIST")
            })
            .collect();
        if allocations.len() != 1 {
            return Err(Error::Validation(
                "each item needs exactly one accounting allocation".into(),
            ));
        }
        let allocation = allocations[0];
        invoice_list_fields(
            allocation,
            &["LEDGERNAME", "AMOUNT", "ISDEEMEDPOSITIVE"],
            label,
            issues,
        );
        let ledger = invoice_field(allocation, "LEDGERNAME")?;
        let (_, category) = ledgers
            .get(&normalized(&ledger))
            .ok_or_else(|| Error::Validation(format!("ledger {ledger} is unavailable")))?;
        if (kind == InvoiceKind::Sale && *category != Category::Income)
            || (kind == InvoiceKind::Purchase
                && !matches!(category, Category::Expense | Category::Asset))
        {
            return Err(Error::Validation(
                "item accounting ledger has an incompatible category".into(),
            ));
        }
        if xml_amount(&invoice_field(allocation, "AMOUNT")?)? != expected {
            return Err(Error::Validation(
                "item accounting allocation differs from the item amount".into(),
            ));
        }
        let locations_in_line: Vec<_> = item_node
            .children()
            .filter(|n| {
                n.is_element()
                    && local_name(*n).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
                    && meaningful(*n)
            })
            .collect();
        if locations_in_line.len() > 1 {
            return Err(Error::Validation(
                "multiple location or batch allocations are not supported".into(),
            ));
        }
        let mut batch_name = None;
        let location = if let Some(location_node) = locations_in_line.first() {
            let location_node = *location_node;
            invoice_list_fields(
                location_node,
                &[
                    "GODOWNNAME",
                    "BATCHNAME",
                    "ACTUALQTY",
                    "BILLEDQTY",
                    "AMOUNT",
                ],
                label,
                issues,
            );
            batch_name = direct_text(location_node, "BATCHNAME").filter(|value| !value.is_empty());
            if xml_stock_quantity(&invoice_field(location_node, "ACTUALQTY")?, unit)? != actual
                || xml_stock_quantity(&invoice_field(location_node, "BILLEDQTY")?, unit)? != billed
                || xml_amount(&invoice_field(location_node, "AMOUNT")?)? != expected
            {
                return Err(Error::Validation(
                    "location allocation does not match the item".into(),
                ));
            }
            invoice_field(location_node, "GODOWNNAME")?
        } else {
            "Main".into()
        };
        let existing_location_id = *locations.get(&normalized(&location)).ok_or_else(|| {
            Error::Validation(format!("stock location {location} is unavailable"))
        })?;
        let existing_batch_id = batch_name.as_ref().and_then(|batch| {
            batches
                .get(&(normalized(&name), normalized(batch)))
                .copied()
        });
        invoice_side(allocation, "ISDEEMEDPOSITIVE", expected > 0)?;
        if trade
            .as_ref()
            .is_some_and(|existing| normalized(existing) != normalized(&ledger))
        {
            return Err(Error::Validation(
                "multiple trade ledgers are not supported".into(),
            ));
        }
        trade = Some(ledger);
        subtotal += i128::from(amount);
        lines.push(ImportedInvoiceLine {
            item: name,
            existing_item_id: *existing_item_id,
            unit: unit.clone(),
            precision: *precision,
            location,
            existing_location_id,
            batch: batch_name,
            existing_batch_id,
            quantity,
            unit_price,
        });
    }
    let trade = trade.ok_or_else(|| Error::Validation("at least one item is required".into()))?;
    i64::try_from(subtotal).map_err(|_| Error::Validation("invoice amount is too large".into()))?;
    let total = subtotal
        + adjustments
            .iter()
            .map(|adjustment| i128::from(adjustment.amount))
            .sum::<i128>();
    if total <= 0 || i128::from(party_amount).abs() != total {
        return Err(Error::Validation(
            "party amount does not equal items plus ledger adjustments".into(),
        ));
    }
    if normalized(&party_name) == normalized(&trade) {
        return Err(Error::Validation(
            "party and trade ledgers must be different".into(),
        ));
    }
    if adjustments
        .iter()
        .any(|adjustment| normalized(&adjustment.ledger) == normalized(&party_name))
    {
        return Err(Error::Validation(
            "adjustment ledger must differ from the party".into(),
        ));
    }
    let existing_party_id = ledgers[&normalized(&party_name)].0;
    let existing_trade_id = ledgers[&normalized(&trade)].0;
    Ok(ImportedInvoice {
        kind,
        date,
        credit_days,
        reference,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
        party: party_name,
        existing_party_id,
        trade,
        existing_trade_id,
        lines,
        adjustments,
        metadata,
    })
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
fn find_stock_item(conn: &Connection, company: i64, name: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id, name FROM stock_items WHERE company_id = ?1")?;
    let mut found = None;
    for row in stmt.query_map([company], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, candidate) = row?;
        if normalized(&candidate) == normalized(name) {
            if found.is_some() {
                return Err(Error::Validation(format!("Stock item {name} is ambiguous")));
            }
            found = Some(id);
        }
    }
    Ok(found)
}
fn find_stock_location(conn: &Connection, company: i64, name: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id, name FROM stock_locations WHERE company_id = ?1")?;
    let mut found = None;
    for row in stmt.query_map([company], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, candidate) = row?;
        if normalized(&candidate) == normalized(name) {
            if found.is_some() {
                return Err(Error::Validation(format!(
                    "Stock location {name} is ambiguous"
                )));
            }
            found = Some(id);
        }
    }
    Ok(found)
}
fn find_stock_unit(conn: &Connection, company: i64, symbol: &str) -> Result<Option<(i64, u8)>> {
    let mut stmt =
        conn.prepare("SELECT id, symbol, decimal_places FROM stock_units WHERE company_id = ?1")?;
    let mut found = None;
    for row in stmt.query_map([company], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u8>(2)?,
        ))
    })? {
        let (id, name, places) = row?;
        if normalized(&name) == normalized(symbol) {
            if found.is_some() {
                return Err(Error::Validation(format!("Unit {symbol} is ambiguous")));
            }
            found = Some((id, places));
        }
    }
    Ok(found)
}
fn find_stock_group(conn: &Connection, company: i64, name: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id, name FROM stock_groups WHERE company_id = ?1")?;
    let mut found = None;
    for row in stmt.query_map([company], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, candidate) = row?;
        if normalized(&candidate) == normalized(name) {
            if found.is_some() {
                return Err(Error::Validation(format!(
                    "Stock group {name} is ambiguous"
                )));
            }
            found = Some(id);
        }
    }
    Ok(found)
}

impl Store {
    pub fn preview_xml_import(&self, company_id: i64, bytes: &[u8]) -> Result<ImportPlan> {
        self.preview_xml_import_with_opening_date(company_id, bytes, None)
    }
    pub fn preview_xml_import_with_opening_date(
        &self,
        company_id: i64,
        bytes: &[u8],
        opening_date: Option<&str>,
    ) -> Result<ImportPlan> {
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
            opening_date: opening_date
                .filter(|s| !s.trim().is_empty())
                .map(xml_date)
                .transpose()?,
            existing_group_metadata: Vec::new(),
            existing_unit_metadata: Vec::new(),
            existing_stock_group_metadata: Vec::new(),
            existing_location_metadata: Vec::new(),
            groups: Vec::new(),
            accounts: Vec::new(),
            units: Vec::new(),
            stock_groups: Vec::new(),
            locations: Vec::new(),
            items: Vec::new(),
            transactions: Vec::new(),
            invoices: Vec::new(),
            issues: Vec::new(),
        };
        let mut seen_metadata = HashSet::<(String, String, String)>::new();
        {
            let mut stmt = self.conn.prepare("SELECT kind, guid, remote_id, master_id FROM imported_record_metadata WHERE company_id = ?1")?;
            let rows = stmt.query_map([company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (kind, guid, remote, master) = row?;
                for (key, value) in [("guid", guid), ("remote", remote), ("master", master)] {
                    if !value.is_empty() {
                        seen_metadata.insert((kind.clone(), key.to_owned(), normalized(&value)));
                    }
                }
            }
        }
        for node in doc.descendants().filter(|n| {
            n.is_element()
                && n.parent().is_some_and(|p| {
                    p.is_element()
                        && (local_name(p).to_ascii_uppercase().ends_with("MESSAGE")
                            || local_name(p).eq_ignore_ascii_case("COLLECTION"))
                })
        }) {
            if ![
                "GROUP",
                "LEDGER",
                "VOUCHER",
                "UNIT",
                "STOCKGROUP",
                "STOCKITEM",
                "GODOWN",
            ]
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
        let mut pending_groups = Vec::<(String, String, SourceMetadata)>::new();
        let mut seen_input_groups = HashSet::<String>::new();
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
            if !seen_input_groups.insert(normalized(&name)) {
                plan.issues.push(format!("Group {name}: duplicate name"));
                continue;
            }
            let metadata = source_metadata(node, &format!("Group {name}"), &mut plan.issues);
            register_metadata(
                "group",
                &format!("Group {name}"),
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            let parent = direct_text(node, "PARENT").unwrap_or_default();
            if let Some(existing) = existing_groups
                .iter()
                .find(|g| normalized(&g.name) == normalized(&name))
            {
                let expected_parent = existing
                    .parent_id
                    .and_then(|id| existing_groups.iter().find(|g| g.id == id))
                    .map(|g| normalized(&g.name));
                let primary_root = parent.eq_ignore_ascii_case("Primary")
                    && existing
                        .parent_id
                        .and_then(|id| existing_groups.iter().find(|g| g.id == id))
                        .is_some_and(|g| g.parent_id.is_none() && g.name == g.category.as_str());
                if !parent.is_empty()
                    && expected_parent.as_deref() != Some(normalized(&parent).as_str())
                    && !primary_root
                {
                    plan.issues.push(format!(
                        "Group {name}: existing parent differs from the imported parent"
                    ));
                }
                if !metadata.is_empty() {
                    plan.existing_group_metadata.push((existing.id, metadata));
                }
                continue;
            }
            if parent.is_empty() {
                plan.issues.push(format!("Group {name}: parent is missing"));
                continue;
            }
            if pending_groups
                .iter()
                .any(|(n, _, _)| normalized(n) == normalized(&name))
            {
                plan.issues.push(format!("Group {name}: duplicate name"));
                continue;
            }
            pending_groups.push((name, parent, metadata));
        }
        while !pending_groups.is_empty() {
            let mut progress = false;
            let mut unresolved = Vec::new();
            for (name, parent, metadata) in pending_groups {
                if let Some(&category) = known_groups.get(&normalized(&parent)) {
                    known_groups.insert(normalized(&name), category);
                    plan.groups.push(Group {
                        name,
                        parent,
                        category,
                        metadata,
                    });
                    progress = true;
                } else {
                    unresolved.push((name, parent, metadata));
                }
            }
            if !progress {
                for (name, parent, _) in unresolved {
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
            let metadata = source_metadata(node, &label, &mut plan.issues);
            register_metadata(
                "ledger",
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
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
            let opening = match direct_text(node, "OPENINGBALANCE").filter(|s| !s.is_empty()) {
                Some(value) => match xml_amount(&value) {
                    Ok(0) => None,
                    Ok(amount) => {
                        if plan.opening_date.is_none() {
                            plan.issues
                                .push(format!("Ledger {name}: provide an opening date"));
                        }
                        Some(amount)
                    }
                    Err(_) => {
                        plan.issues
                            .push(format!("Ledger {name}: opening balance is invalid"));
                        None
                    }
                },
                None => None,
            };
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
                opening,
                metadata,
            });
        }
        let existing_units = self.stock_units(company_id)?;
        let mut known_units = HashMap::<String, u8>::new();
        for unit in &existing_units {
            if known_units
                .insert(normalized(&unit.symbol), unit.decimal_places)
                .is_some()
            {
                plan.issues
                    .push(format!("Unit {} is ambiguous", unit.symbol));
            }
        }
        let mut seen_units = HashSet::new();
        for node in doc
            .descendants()
            .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("UNIT"))
        {
            let Some(symbol) = object_name(node) else {
                plan.issues.push("A unit has no unambiguous symbol".into());
                continue;
            };
            let label = format!("Unit {symbol}");
            check_fields(
                node,
                &[
                    "NAME",
                    "ORIGINALNAME",
                    "ISSIMPLEUNIT",
                    "DECIMALPLACES",
                    "LANGUAGENAME.LIST",
                ],
                &label,
                &mut plan.issues,
            );
            check_language_names(node, &symbol, &label, &mut plan.issues);
            if !seen_units.insert(normalized(&symbol)) {
                plan.issues.push(format!("{label}: duplicate symbol"));
                continue;
            }
            let metadata = source_metadata(node, &label, &mut plan.issues);
            register_metadata(
                "stockunit",
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            if !direct_text(node, "ISSIMPLEUNIT").is_some_and(|s| s.eq_ignore_ascii_case("yes")) {
                plan.issues.push(format!(
                    "{label}: compound or unspecified units are not supported"
                ));
            }
            let places = match direct_text(node, "DECIMALPLACES")
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|n| *n <= 3)
            {
                Some(v) => v,
                None => {
                    plan.issues
                        .push(format!("{label}: decimal places must be between 0 and 3"));
                    continue;
                }
            };
            let formal_name = direct_text(node, "ORIGINALNAME").unwrap_or_default();
            if let Some(existing) = existing_units
                .iter()
                .find(|u| normalized(&u.symbol) == normalized(&symbol))
            {
                if existing.decimal_places != places
                    || (!formal_name.is_empty() && existing.formal_name != formal_name)
                {
                    plan.issues
                        .push(format!("{label}: existing unit has different details"));
                }
                if !metadata.is_empty() {
                    plan.existing_unit_metadata.push((existing.id, metadata));
                }
            } else {
                known_units.insert(normalized(&symbol), places);
                plan.units.push(Unit {
                    symbol,
                    formal_name,
                    decimal_places: places,
                    metadata,
                });
            }
        }
        let existing_stock_groups = self.stock_groups(company_id)?;
        let mut known_stock_groups = HashSet::<String>::new();
        for group in &existing_stock_groups {
            if !known_stock_groups.insert(normalized(&group.name)) {
                plan.issues
                    .push(format!("Stock group {} is ambiguous", group.name));
            }
        }
        let mut input_stock_groups = HashSet::new();
        let mut pending_stock_groups = Vec::<(String, String, SourceMetadata)>::new();
        for node in doc
            .descendants()
            .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("STOCKGROUP"))
        {
            let Some(name) = object_name(node) else {
                plan.issues
                    .push("A stock group has no unambiguous name".into());
                continue;
            };
            let label = format!("Stock group {name}");
            check_fields(
                node,
                &["NAME", "PARENT", "LANGUAGENAME.LIST"],
                &label,
                &mut plan.issues,
            );
            check_language_names(node, &name, &label, &mut plan.issues);
            if !input_stock_groups.insert(normalized(&name)) {
                plan.issues.push(format!("{label}: duplicate name"));
                continue;
            }
            let metadata = source_metadata(node, &label, &mut plan.issues);
            register_metadata(
                "stockgroup",
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            let parent = direct_text(node, "PARENT")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Primary".into());
            if let Some(existing) = existing_stock_groups
                .iter()
                .find(|g| normalized(&g.name) == normalized(&name))
            {
                let actual = existing
                    .parent_id
                    .and_then(|id| existing_stock_groups.iter().find(|g| g.id == id))
                    .map(|g| normalized(&g.name));
                if actual.as_deref() != Some(normalized(&parent).as_str())
                    && !(actual.is_none()
                        && normalized(&name) == "primary"
                        && normalized(&parent) == "primary")
                {
                    plan.issues
                        .push(format!("{label}: existing parent differs"));
                }
                if !metadata.is_empty() {
                    plan.existing_stock_group_metadata
                        .push((existing.id, metadata));
                }
            } else {
                pending_stock_groups.push((name, parent, metadata));
            }
        }
        while !pending_stock_groups.is_empty() {
            let mut progress = false;
            let mut unresolved = Vec::new();
            for (name, parent, metadata) in pending_stock_groups {
                if known_stock_groups.contains(&normalized(&parent)) {
                    known_stock_groups.insert(normalized(&name));
                    plan.stock_groups.push(StockGroupImport {
                        name,
                        parent,
                        metadata,
                    });
                    progress = true;
                } else {
                    unresolved.push((name, parent, metadata));
                }
            }
            if !progress {
                for (name, parent, _) in unresolved {
                    plan.issues.push(format!(
                        "Stock group {name}: parent {parent} is missing or cyclic"
                    ));
                }
                break;
            }
            pending_stock_groups = unresolved;
        }
        let existing_locations = self.stock_locations(company_id)?;
        let mut known_locations = HashMap::<String, Option<i64>>::new();
        for location in &existing_locations {
            if known_locations
                .insert(normalized(&location.name), Some(location.id))
                .is_some()
            {
                plan.issues
                    .push(format!("Stock location {} is ambiguous", location.name));
            }
        }
        let mut input_locations = HashSet::new();
        for node in doc.descendants().filter(|n| {
            n.is_element()
                && local_name(*n).eq_ignore_ascii_case("GODOWN")
                && !n
                    .ancestors()
                    .any(|a| a.is_element() && local_name(a).eq_ignore_ascii_case("VOUCHER"))
        }) {
            let Some(name) = object_name(node) else {
                plan.issues
                    .push("A stock location has no unambiguous name".into());
                continue;
            };
            let label = format!("Stock location {name}");
            check_fields(
                node,
                &["NAME", "PARENT", "LANGUAGENAME.LIST"],
                &label,
                &mut plan.issues,
            );
            check_language_names(node, &name, &label, &mut plan.issues);
            if !input_locations.insert(normalized(&name)) {
                plan.issues.push(format!("{label}: duplicate name"));
                continue;
            }
            let parent = direct_text(node, "PARENT").unwrap_or_default();
            if normalized(&name) == "primary"
                || (!parent.is_empty() && normalized(&parent) != "primary")
            {
                plan.issues.push(format!(
                    "{label}: nested or root location records are not supported"
                ));
            }
            let metadata = source_metadata(node, &label, &mut plan.issues);
            register_metadata(
                "stocklocation",
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            if let Some(id) = known_locations.get(&normalized(&name)).copied().flatten() {
                if !metadata.is_empty() {
                    plan.existing_location_metadata.push((id, metadata));
                }
            } else {
                known_locations.insert(normalized(&name), None);
                plan.locations.push(StockLocationImport { name, metadata });
            }
        }
        let mut known_items = HashSet::<String>::new();
        for item in self.stock_items(company_id)? {
            if !known_items.insert(normalized(&item.name)) {
                plan.issues
                    .push(format!("Stock item {} is ambiguous", item.name));
            }
        }
        for node in doc
            .descendants()
            .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("STOCKITEM"))
        {
            let Some(name) = object_name(node) else {
                plan.issues
                    .push("A stock item has no unambiguous name".into());
                continue;
            };
            let label = format!("Stock item {name}");
            check_fields(
                node,
                &[
                    "NAME",
                    "NAME.LIST",
                    "LANGUAGENAME.LIST",
                    "PARENT",
                    "BASEUNITS",
                    "DESCRIPTION",
                    "OPENINGBALANCE",
                    "OPENINGRATE",
                    "OPENINGVALUE",
                    "BATCHALLOCATIONS.LIST",
                ],
                &label,
                &mut plan.issues,
            );
            check_language_names(node, &name, &label, &mut plan.issues);
            let names = profile_list_values(node, "NAME.LIST", "NAME", &label, &mut plan.issues);
            if !names.is_empty() && (names.len() != 1 || names[0] != name) {
                plan.issues
                    .push(format!("{label}: aliases are not supported"));
            }
            let metadata = source_metadata(node, &label, &mut plan.issues);
            register_metadata(
                "stockitem",
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            let unit = direct_text(node, "BASEUNITS").unwrap_or_default();
            let precision = match known_units.get(&normalized(&unit)) {
                Some(v) => *v,
                None => {
                    plan.issues
                        .push(format!("{label}: unit {unit} is not available"));
                    continue;
                }
            };
            let group = direct_text(node, "PARENT")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Primary".into());
            if !known_stock_groups.contains(&normalized(&group)) {
                plan.issues
                    .push(format!("{label}: stock group {group} is not available"));
            }
            if !known_items.insert(normalized(&name)) {
                plan.issues.push(format!("{label}: duplicate name"));
            }
            if direct_text(node, "OPENINGVALUE")
                .is_some_and(|v| !v.is_empty() && !matches!(xml_amount(&v), Ok(0)))
                || direct_text(node, "OPENINGRATE")
                    .is_some_and(|v| !unvalued_opening_rate(&v, &unit))
            {
                plan.issues
                    .push(format!("{label}: opening valuation is not supported"));
            }
            let top_opening = match direct_text(node, "OPENINGBALANCE").filter(|v| !v.is_empty()) {
                Some(value) => match xml_stock_quantity(&value, &unit) {
                    Ok(quantity) => Some(quantity),
                    Err(_) => {
                        plan.issues.push(format!(
                            "{label}: opening quantity is invalid or uses another unit"
                        ));
                        None
                    }
                },
                None => None,
            };
            let allocations: Vec<_> = node
                .children()
                .filter(|n| {
                    n.is_element()
                        && local_name(*n).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
                        && meaningful(*n)
                })
                .collect();
            let mut openings = Vec::new();
            let mut batch_dates = HashMap::<String, Option<String>>::new();
            for allocation in &allocations {
                invoice_list_fields(
                    *allocation,
                    &[
                        "NAME",
                        "BATCHNAME",
                        "GODOWNNAME",
                        "MFDON",
                        "OPENINGBALANCE",
                        "OPENINGVALUE",
                        "OPENINGRATE",
                    ],
                    &label,
                    &mut plan.issues,
                );
                let first = direct_text(*allocation, "NAME").filter(|v| !v.is_empty());
                let second = direct_text(*allocation, "BATCHNAME").filter(|v| !v.is_empty());
                if first
                    .as_ref()
                    .zip(second.as_ref())
                    .is_some_and(|(a, b)| a != b)
                {
                    plan.issues.push(format!("{label}: batch names conflict"));
                }
                let batch = second.or(first);
                let location = direct_text(*allocation, "GODOWNNAME")
                    .filter(|v| !v.is_empty())
                    .unwrap_or_default();
                let existing_location_id = match known_locations.get(&normalized(&location)) {
                    Some(id) => *id,
                    None => {
                        plan.issues
                            .push(format!("{label}: stock location {location} is unavailable"));
                        None
                    }
                };
                if direct_text(*allocation, "OPENINGVALUE")
                    .is_some_and(|v| !v.is_empty() && !matches!(xml_amount(&v), Ok(0)))
                    || direct_text(*allocation, "OPENINGRATE")
                        .is_some_and(|v| !unvalued_opening_rate(&v, &unit))
                {
                    plan.issues
                        .push(format!("{label}: batch opening valuation is not supported"));
                }
                let manufactured = match direct_text(*allocation, "MFDON").filter(|v| !v.is_empty())
                {
                    Some(value) => match xml_date(&value) {
                        Ok(date) => Some(date),
                        Err(_) => {
                            plan.issues
                                .push(format!("{label}: invalid batch manufacture date"));
                            None
                        }
                    },
                    None => None,
                };
                if batch.is_none() && manufactured.is_some() {
                    plan.issues
                        .push(format!("{label}: batch date requires a batch name"));
                }
                if let Some(batch) = &batch {
                    let key = normalized(batch);
                    let entry = batch_dates
                        .entry(key)
                        .or_insert_with(|| manufactured.clone());
                    if entry.is_some() && manufactured.is_some() && *entry != manufactured {
                        plan.issues
                            .push(format!("{label}: batch manufacture dates conflict"));
                    }
                    if entry.is_none() {
                        *entry = manufactured.clone();
                    }
                }
                let quantity =
                    match direct_text(*allocation, "OPENINGBALANCE").filter(|v| !v.is_empty()) {
                        Some(value) => match xml_stock_quantity(&value, &unit) {
                            Ok(value) => value,
                            Err(_) => {
                                plan.issues
                                    .push(format!("{label}: invalid batch opening quantity"));
                                0
                            }
                        },
                        None => {
                            plan.issues
                                .push(format!("{label}: batch opening quantity is required"));
                            0
                        }
                    };
                openings.push(StockOpening {
                    location,
                    existing_location_id,
                    batch,
                    manufactured_on: manufactured,
                    quantity,
                });
            }
            for opening in &mut openings {
                if let Some(batch) = &opening.batch {
                    opening.manufactured_on =
                        batch_dates.get(&normalized(batch)).cloned().flatten();
                }
            }
            if allocations.is_empty() {
                if let Some(quantity) = top_opening.filter(|quantity| *quantity != 0) {
                    let existing_location_id = known_locations.get("main").copied().flatten();
                    if !known_locations.contains_key("main") {
                        plan.issues
                            .push(format!("{label}: default stock location is missing"));
                    }
                    openings.push(StockOpening {
                        location: "Main".into(),
                        existing_location_id,
                        batch: None,
                        manufactured_on: None,
                        quantity,
                    });
                }
            } else {
                let sum: i128 = openings
                    .iter()
                    .map(|opening| i128::from(opening.quantity))
                    .sum();
                if top_opening.map(i128::from) != Some(sum) {
                    plan.issues.push(format!(
                        "{label}: batch quantities do not match the opening quantity"
                    ));
                }
            }
            for opening in &openings {
                if opening.quantity % 10_i64.pow(u32::from(3 - precision)) != 0 {
                    plan.issues
                        .push(format!("{label}: opening quantity exceeds unit precision"));
                }
                if opening.quantity != 0 && plan.opening_date.is_none() {
                    plan.issues
                        .push(format!("{label}: provide an opening date"));
                }
            }
            plan.items.push(Item {
                name,
                unit,
                unit_precision: precision,
                group,
                description: direct_text(node, "DESCRIPTION").unwrap_or_default(),
                openings,
                metadata,
            });
        }
        let mut invoice_ledgers: HashMap<String, (Option<i64>, Category)> = self
            .ledgers(company_id)?
            .into_iter()
            .map(|l| (normalized(&l.name), (Some(l.id), l.category)))
            .collect();
        invoice_ledgers.extend(
            plan.accounts
                .iter()
                .map(|a| (normalized(&a.name), (None, a.category))),
        );
        let mut invoice_items = HashMap::<String, (Option<i64>, String, u8)>::new();
        let mut invoice_batches = HashMap::<(String, String), i64>::new();
        for item in self.stock_items(company_id)? {
            for batch in self.stock_batches(company_id, item.id)? {
                if invoice_batches
                    .insert((normalized(&item.name), normalized(&batch.name)), batch.id)
                    .is_some()
                {
                    plan.issues.push(format!(
                        "Batch {} for item {} is ambiguous",
                        batch.name, item.name
                    ));
                }
            }
            if let Some(precision) = known_units.get(&normalized(&item.unit)) {
                invoice_items.insert(
                    normalized(&item.name),
                    (Some(item.id), item.unit, *precision),
                );
            }
        }
        invoice_items.extend(plan.items.iter().map(|i| {
            (
                normalized(&i.name),
                (None, i.unit.clone(), i.unit_precision),
            )
        }));
        let mut invoice_references = HashSet::<(String, String, String)>::new();
        {
            let mut stmt = self.conn.prepare("SELECT i.kind, l.name, v.reference FROM invoices i JOIN vouchers v ON v.id = i.voucher_id JOIN ledgers l ON l.id = i.party_ledger_id WHERE i.company_id = ?1")?;
            for row in stmt.query_map([company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (kind, party, reference) = row?;
                invoice_references.insert((
                    normalized(&kind),
                    normalized(&party),
                    normalized(&reference),
                ));
            }
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
            let metadata = source_metadata(node, &label, &mut plan.issues);
            register_metadata(
                "voucher",
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            if direct_text(node, "ISINVOICE").is_some_and(|v| v.eq_ignore_ascii_case("yes")) {
                match parse_imported_invoice(
                    node,
                    &label,
                    metadata,
                    &invoice_ledgers,
                    &invoice_items,
                    &known_locations,
                    &invoice_batches,
                    &mut plan.issues,
                ) {
                    Ok(invoice) => {
                        let voucher_kind = if invoice.kind == InvoiceKind::Sale {
                            VoucherKind::Sales
                        } else {
                            VoucherKind::Purchase
                        };
                        if !references.insert((
                            normalized(voucher_kind.as_str()),
                            invoice.date.clone(),
                            normalized(&invoice.reference),
                        )) {
                            plan.issues.push(format!("{label}: a voucher with the same type, date and number already exists"));
                        }
                        if !invoice_references.insert((
                            normalized(invoice.kind.as_str()),
                            normalized(&invoice.party),
                            normalized(&invoice.reference),
                        )) {
                            plan.issues.push(format!("{label}: an invoice with the same type, party and reference already exists"));
                        }
                        plan.invoices.push(invoice);
                    }
                    Err(e) => plan.issues.push(format!("{label}: {e}")),
                }
                continue;
            }
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
            if let Some(attr) = node
                .attributes()
                .find(|a| a.name().eq_ignore_ascii_case("VCHTYPE"))
            {
                if parse_kind(attr.value()) != Some(kind) {
                    plan.issues
                        .push(format!("{label}: conflicting voucher type"));
                }
            }
            let date = match direct_text(node, "DATE").and_then(|v| xml_date(&v).ok()) {
                Some(v) => v,
                None => {
                    plan.issues
                        .push(format!("{label}: date is missing or invalid"));
                    continue;
                }
            };
            if let Some(attr) = node
                .attributes()
                .find(|a| a.name().eq_ignore_ascii_case("DATE"))
            {
                if xml_date(attr.value()).ok().as_deref() != Some(date.as_str()) {
                    plan.issues
                        .push(format!("{label}: conflicting voucher date"));
                }
            }
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
                metadata,
            });
        }
        for node in doc.descendants().filter(|n| {
            n.is_element()
                && ["VOUCHERTYPE", "STOCKCATEGORY"]
                    .iter()
                    .any(|tag| local_name(*n).eq_ignore_ascii_case(tag))
        }) {
            plan.issues
                .push(format!("{} records are not supported", local_name(node)));
        }
        if plan.groups.is_empty()
            && plan.accounts.is_empty()
            && plan.transactions.is_empty()
            && plan.invoices.is_empty()
            && plan.units.is_empty()
            && plan.stock_groups.is_empty()
            && plan.locations.is_empty()
            && plan.items.is_empty()
            && plan.existing_group_metadata.is_empty()
            && plan.existing_unit_metadata.is_empty()
            && plan.existing_stock_group_metadata.is_empty()
            && plan.existing_location_metadata.is_empty()
            && plan.issues.is_empty()
        {
            plan.issues.push("No supported records were found".into());
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
        for (id, metadata) in &plan.existing_location_metadata {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_locations WHERE id = ?1 AND company_id = ?2)",
                params![id, plan.company_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "A stock location changed since preview".into(),
                ));
            }
            save_metadata(&tx, plan.company_id, "stocklocation", *id, metadata)?;
        }
        for location in &plan.locations {
            if find_stock_location(&tx, plan.company_id, &location.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Stock location {} already exists",
                    location.name
                )));
            }
            tx.execute(
                "INSERT INTO stock_locations(company_id, name) VALUES (?1, ?2)",
                params![plan.company_id, location.name],
            )?;
            save_metadata(
                &tx,
                plan.company_id,
                "stocklocation",
                tx.last_insert_rowid(),
                &location.metadata,
            )?;
        }
        for (id, metadata) in &plan.existing_unit_metadata {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_units WHERE company_id = ?1 AND id = ?2)",
                params![plan.company_id, id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation("A unit changed since preview".into()));
            }
            save_metadata(&tx, plan.company_id, "stockunit", *id, metadata)?;
        }
        for (id, metadata) in &plan.existing_stock_group_metadata {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_groups WHERE company_id = ?1 AND id = ?2)",
                params![plan.company_id, id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "A stock group changed since preview".into(),
                ));
            }
            save_metadata(&tx, plan.company_id, "stockgroup", *id, metadata)?;
        }
        for unit in &plan.units {
            if find_stock_unit(&tx, plan.company_id, &unit.symbol)?.is_some() {
                return Err(Error::Validation(format!(
                    "Unit {} already exists",
                    unit.symbol
                )));
            }
            tx.execute("INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places) VALUES (?1, ?2, ?3, ?4)", params![plan.company_id, unit.symbol, unit.formal_name, unit.decimal_places])?;
            save_metadata(
                &tx,
                plan.company_id,
                "stockunit",
                tx.last_insert_rowid(),
                &unit.metadata,
            )?;
        }
        for group in &plan.stock_groups {
            if find_stock_group(&tx, plan.company_id, &group.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Stock group {} already exists",
                    group.name
                )));
            }
            let parent =
                find_stock_group(&tx, plan.company_id, &group.parent)?.ok_or_else(|| {
                    Error::Validation(format!("Stock group {} is missing", group.parent))
                })?;
            tx.execute(
                "INSERT INTO stock_groups(company_id, name, parent_id) VALUES (?1, ?2, ?3)",
                params![plan.company_id, group.name, parent],
            )?;
            save_metadata(
                &tx,
                plan.company_id,
                "stockgroup",
                tx.last_insert_rowid(),
                &group.metadata,
            )?;
        }
        for (id, metadata) in &plan.existing_group_metadata {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM account_groups WHERE company_id = ?1 AND id = ?2)",
                params![plan.company_id, id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "An account group changed since preview".into(),
                ));
            }
            save_metadata(&tx, plan.company_id, "group", *id, metadata)?;
        }
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
            save_metadata(
                &tx,
                plan.company_id,
                "group",
                tx.last_insert_rowid(),
                &group.metadata,
            )?;
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
            let ledger_id = tx.last_insert_rowid();
            save_metadata(&tx, plan.company_id, "ledger", ledger_id, &account.metadata)?;
            insert_new_profile(&tx, plan.company_id, ledger_id, &account.profile)?;
            if let Some(amount) = account.opening {
                let ledger_id =
                    find_ledger(&tx, plan.company_id, &account.name)?.ok_or_else(|| {
                        Error::Validation(format!("Ledger {} is missing", account.name))
                    })?;
                let date = plan
                    .opening_date
                    .as_deref()
                    .ok_or_else(|| Error::Validation("Opening date is required".into()))?;
                crate::opening::save_opening_in_transaction(
                    &tx,
                    plan.company_id,
                    ledger_id,
                    date,
                    amount,
                )?;
            }
        }
        let mut created_batches = HashMap::<(i64, String), i64>::new();
        for item in &plan.items {
            let (unit_id, precision) = find_stock_unit(&tx, plan.company_id, &item.unit)?
                .ok_or_else(|| Error::Validation(format!("Unit {} is missing", item.unit)))?;
            if precision != item.unit_precision {
                return Err(Error::Validation(format!(
                    "Unit {} changed since preview",
                    item.unit
                )));
            }
            let group_id =
                find_stock_group(&tx, plan.company_id, &item.group)?.ok_or_else(|| {
                    Error::Validation(format!("Stock group {} is missing", item.group))
                })?;
            let item_id = create_stock_item_on_connection(
                &tx,
                plan.company_id,
                &item.name,
                unit_id,
                group_id,
                &item.description,
            )?;
            save_metadata(&tx, plan.company_id, "stockitem", item_id, &item.metadata)?;
            for opening in &item.openings {
                let location = find_stock_location(&tx, plan.company_id, &opening.location)?
                    .ok_or_else(|| {
                        Error::Validation(format!("Stock location {} is missing", opening.location))
                    })?;
                if opening
                    .existing_location_id
                    .is_some_and(|expected| expected != location)
                {
                    return Err(Error::Validation(format!(
                        "Stock location {} changed since preview",
                        opening.location
                    )));
                }
                let batch = if let Some(name) = &opening.batch {
                    let key = (item_id, normalized(name));
                    let id = if let Some(id) = created_batches.get(&key) {
                        *id
                    } else {
                        let id = create_stock_batch_on_connection(
                            &tx,
                            plan.company_id,
                            item_id,
                            name,
                            opening.manufactured_on.as_deref(),
                            None,
                        )?;
                        created_batches.insert(key, id);
                        id
                    };
                    Some(id)
                } else {
                    None
                };
                if opening.quantity != 0 {
                    check_quantity_precision(&tx, plan.company_id, item_id, opening.quantity)?;
                    let date = plan
                        .opening_date
                        .as_deref()
                        .ok_or_else(|| Error::Validation("Opening date is required".into()))?;
                    tx.execute("INSERT INTO stock_adjustments(company_id, item_id, date, quantity, reason, location_id, batch_id) VALUES (?1, ?2, ?3, ?4, 'Imported opening quantity', ?5, ?6)", params![plan.company_id, item_id, date, opening.quantity, location, batch])?;
                }
            }
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
            crate::opening::check_no_control_postings(&tx, plan.company_id, &postings)?;
            tx.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![plan.company_id, voucher.kind.as_str(), voucher.reference, voucher.date, voucher.narration])?;
            let id = tx.last_insert_rowid();
            save_metadata(&tx, plan.company_id, "voucher", id, &voucher.metadata)?;
            for posting in postings {
                tx.execute(
                    "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
                    params![id, posting.ledger_id, posting.amount],
                )?;
            }
        }
        for invoice in &plan.invoices {
            let voucher_kind = if invoice.kind == InvoiceKind::Sale {
                VoucherKind::Sales
            } else {
                VoucherKind::Purchase
            };
            let mut stmt =
                tx.prepare("SELECT kind, date, reference FROM vouchers WHERE company_id = ?1")?;
            for row in stmt.query_map([plan.company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (kind, date, reference) = row?;
                if normalized(&kind) == normalized(voucher_kind.as_str())
                    && xml_date(&date)? == invoice.date
                    && normalized(&reference) == normalized(&invoice.reference)
                {
                    return Err(Error::Validation(format!(
                        "Voucher {} already exists",
                        invoice.reference
                    )));
                }
            }
            drop(stmt);
            let resolve_ledger = |name: &str, expected: Option<i64>| -> Result<i64> {
                let id = find_ledger(&tx, plan.company_id, name)?
                    .ok_or_else(|| Error::Validation(format!("Ledger {name} is missing")))?;
                if expected.is_some_and(|previous| previous != id) {
                    return Err(Error::Validation(format!(
                        "Ledger {name} changed since preview"
                    )));
                }
                Ok(id)
            };
            let party_id = resolve_ledger(&invoice.party, invoice.existing_party_id)?;
            let trade_id = resolve_ledger(&invoice.trade, invoice.existing_trade_id)?;
            let mut lines = Vec::with_capacity(invoice.lines.len());
            for line in &invoice.lines {
                let item_id =
                    find_stock_item(&tx, plan.company_id, &line.item)?.ok_or_else(|| {
                        Error::Validation(format!("Stock item {} is missing", line.item))
                    })?;
                if line
                    .existing_item_id
                    .is_some_and(|previous| previous != item_id)
                {
                    return Err(Error::Validation(format!(
                        "Stock item {} changed since preview",
                        line.item
                    )));
                }
                let (unit, precision): (String, u8) = tx.query_row("SELECT u.symbol, u.decimal_places FROM stock_items i JOIN stock_units u ON u.id = i.unit_id AND u.company_id = i.company_id WHERE i.id = ?1 AND i.company_id = ?2", params![item_id, plan.company_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                if normalized(&unit) != normalized(&line.unit) || precision != line.precision {
                    return Err(Error::Validation(format!(
                        "Unit for {} changed since preview",
                        line.item
                    )));
                }
                let batch_id = if let Some(name) = &line.batch {
                    let key = (item_id, normalized(name));
                    let existing = find_stock_batch(&tx, plan.company_id, item_id, name)?;
                    let id = if let Some(expected) = line.existing_batch_id {
                        if existing != Some(expected) {
                            return Err(Error::Validation(format!(
                                "Batch {name} changed since preview"
                            )));
                        }
                        expected
                    } else if let Some(created) = created_batches.get(&key) {
                        if existing != Some(*created) {
                            return Err(Error::Validation(format!(
                                "Batch {name} changed since preview"
                            )));
                        }
                        *created
                    } else {
                        if existing.is_some() {
                            return Err(Error::Validation(format!(
                                "Batch {name} appeared since preview"
                            )));
                        }
                        let id = create_stock_batch_on_connection(
                            &tx,
                            plan.company_id,
                            item_id,
                            name,
                            None,
                            None,
                        )?;
                        created_batches.insert(key, id);
                        id
                    };
                    Some(id)
                } else {
                    None
                };
                lines.push(InvoiceLine {
                    item_id,
                    location_id: {
                        let id = find_stock_location(&tx, plan.company_id, &line.location)?
                            .ok_or_else(|| {
                                Error::Validation(format!(
                                    "Stock location {} is missing",
                                    line.location
                                ))
                            })?;
                        if line
                            .existing_location_id
                            .is_some_and(|previous| previous != id)
                        {
                            return Err(Error::Validation(format!(
                                "Stock location {} changed since preview",
                                line.location
                            )));
                        }
                        id
                    },
                    batch_id,
                    quantity: line.quantity,
                    unit_price: line.unit_price,
                });
            }
            let draft = InvoiceDraft {
                kind: invoice.kind,
                date: invoice.date.clone(),
                due_date: None,
                credit_days: invoice.credit_days,
                reference: invoice.reference.clone(),
                narration: invoice.narration.clone(),
                party_ledger_id: party_id,
                trade_ledger_id: trade_id,
                tax_ledger_id: None,
                tax_amount: 0,
                lines,
                adjustments: invoice
                    .adjustments
                    .iter()
                    .map(|adjustment| {
                        Ok(InvoiceAdjustment {
                            ledger_id: resolve_ledger(
                                &adjustment.ledger,
                                adjustment.existing_ledger_id,
                            )?,
                            amount: adjustment.amount,
                            description: String::new(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            };
            let (_, voucher_id) = insert_invoice_on_connection(&tx, plan.company_id, &draft)?;
            save_metadata(
                &tx,
                plan.company_id,
                "voucher",
                voucher_id,
                &invoice.metadata,
            )?;
        }
        tx.commit()?;
        Ok(ImportCounts {
            groups: plan.groups.len(),
            ledgers: plan.accounts.len(),
            vouchers: plan.transactions.len(),
            invoices: plan.invoices.len(),
            units: plan.units.len(),
            stock_groups: plan.stock_groups.len(),
            locations: plan.locations.len(),
            items: plan.items.len(),
        })
    }
}
