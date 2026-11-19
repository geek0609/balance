//! Import of a limited accounting XML interchange format.
//! Unsupported or ambiguous data is reported before any database writes.
use crate::advances::{
    advance_allocation_state, insert_advance_allocation, insert_party_advance, read_party_advances,
    AdvanceDraft, AdvanceSourceBill,
};
use crate::bill_splits::BillSplitDraft;
use crate::contacts::{insert_new_profile, LedgerProfile};
use crate::inventory::{
    check_quantity_precision, create_stock_batch_on_connection, create_stock_item_on_connection,
    discounted_line_amount, find_stock_batch, insert_direct_invoice,
    insert_settlement_on_connection, insert_stock_adjustment, invoice_total, parse_quantity,
    BillSettlementAllocation, BillSettlementTarget, InvoiceAdjustment, InvoiceAllocation,
    InvoiceDiscount, InvoiceDraft, InvoiceEntryMode, InvoiceKind, InvoiceLine, StockInvoicePosting,
};
use crate::opening_bills::OpeningBillDraft;
use crate::{
    insert_voucher_draft, parse_amount, parse_date, Category, Error, Posting, Result, Store,
    VoucherDraft, VoucherKind,
};
use chrono::NaiveDate;
use roxmltree::{Document, Node, ParsingOptions};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::{prelude::ToPrimitive, Decimal, RoundingStrategy};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::str::FromStr;

/// Maximum combined input size for a single XML import preview.
pub const MAX_XML_IMPORT_BYTES: usize = 50 * 1024 * 1024;
const MAX_XML_NODES: u32 = 1_000_000;

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
    opening_bills: Vec<ImportedOpeningBill>,
    credit_days: Option<u32>,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct ImportedOpeningBill {
    kind: InvoiceKind,
    reference: String,
    original_date: String,
    credit_days: Option<u32>,
    amount: i64,
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
    parent: Option<String>,
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
    cost: Option<i64>,
}
#[derive(Debug, Clone)]
struct Line {
    ledger: String,
    amount: i64,
    is_party: bool,
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
    allocations: Vec<ImportedStockAllocation>,
    trade_ledger: String,
    existing_trade_ledger_id: Option<i64>,
    quantity: i64,
    unit_price: i64,
    discount: InvoiceDiscount,
}
#[derive(Debug, Clone)]
struct ImportedStockAllocation {
    location: String,
    existing_location_id: Option<i64>,
    batch: Option<String>,
    existing_batch_id: Option<i64>,
    quantity: i64,
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
    entry_mode: InvoiceEntryMode,
    date: String,
    reference: String,
    bills: Vec<BillSplitDraft>,
    advance_applications: Vec<ImportedAdvanceApplication>,
    advance_bill_reference: Option<String>,
    narration: String,
    party: String,
    existing_party_id: Option<i64>,
    trade: String,
    existing_trade_id: Option<i64>,
    lines: Vec<ImportedInvoiceLine>,
    adjustments: Vec<ImportedInvoiceAdjustment>,
    original_postings: Vec<Line>,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct ImportedAdvanceApplication {
    reference: String,
    amount: i64,
    existing_advance_id: Option<i64>,
    expected_available: i128,
}

#[derive(Debug, Clone)]
struct ImportedBillAllocation {
    reference: String,
    amount: i64,
    existing_target: Option<BillSettlementTarget>,
    expected_outstanding: i128,
    credit_days: Option<u32>,
}
#[derive(Debug, Clone)]
struct ImportedBillUse {
    invoices: Vec<ImportedBillAllocation>,
    advance: Option<ImportedAdvance>,
}
#[derive(Debug, Clone)]
struct ImportedAdvance {
    amount: i64,
    source_bill: AdvanceSourceBill,
}
#[derive(Debug, Clone)]
struct ImportedSettlement {
    kind: InvoiceKind,
    date: String,
    reference: String,
    narration: String,
    party: String,
    bank: String,
    existing_bank_id: Option<i64>,
    existing_party_id: Option<i64>,
    bills: ImportedBillUse,
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
    settlements: Vec<ImportedSettlement>,
    issues: Vec<String>,
}
impl ImportPlan {
    fn check_period_lock(&self, conn: &Connection) -> Result<()> {
        let Some(cutoff) = crate::periods::cutoff(conn, self.company_id)? else {
            return Ok(());
        };
        let openings = self
            .accounts
            .iter()
            .any(|row| row.opening.is_some() || !row.opening_bills.is_empty())
            || self
                .items
                .iter()
                .any(|item| item.openings.iter().any(|row| row.quantity != 0));
        let dates = self
            .transactions
            .iter()
            .map(|row| row.date.as_str())
            .chain(self.invoices.iter().map(|row| row.date.as_str()))
            .chain(self.settlements.iter().map(|row| row.date.as_str()))
            .chain(self.opening_date.as_deref().filter(|_| openings));
        for date in dates {
            if parse_date(date)? <= cutoff {
                return Err(Error::Validation(format!(
                    "Import includes a transaction dated {date}. The company is locked through {cutoff}."
                )));
            }
        }
        Ok(())
    }

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
    pub fn settlement_count(&self) -> usize {
        self.settlements
            .iter()
            .filter(|row| !row.bills.invoices.is_empty())
            .count()
    }
    pub fn advance_count(&self) -> usize {
        self.settlements
            .iter()
            .filter(|row| row.bills.advance.is_some())
            .count()
    }
    pub fn advance_application_count(&self) -> usize {
        self.invoices
            .iter()
            .map(|row| row.advance_applications.len())
            .sum()
    }
    pub fn opening_count(&self) -> usize {
        self.accounts
            .iter()
            .filter(|a| a.opening.is_some() && a.opening_bills.is_empty())
            .count()
    }
    pub fn opening_bill_count(&self) -> usize {
        self.accounts.iter().map(|a| a.opening_bills.len()).sum()
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
    pub fn opening_stock_value(&self) -> (usize, i128) {
        self.items
            .iter()
            .flat_map(|item| &item.openings)
            .filter_map(|opening| opening.cost)
            .fold((0, 0), |(count, value), amount| {
                (count + 1, value + i128::from(amount))
            })
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
                || !self.invoices.is_empty()
                || !self.settlements.is_empty())
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ImportCounts {
    pub groups: usize,
    pub ledgers: usize,
    pub opening_bills: usize,
    pub vouchers: usize,
    pub invoices: usize,
    pub settlements: usize,
    pub advances: usize,
    pub advance_applications: usize,
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
                && ["DATE", "VCHTYPE", "OBJVIEW"]
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
fn party_flag(entry: Node<'_, '_>) -> Result<bool> {
    match direct_text(entry, "ISPARTYLEDGER").as_deref() {
        Some(value) if value.eq_ignore_ascii_case("yes") => Ok(true),
        Some(value) if value.eq_ignore_ascii_case("no") || value.is_empty() => Ok(false),
        None => Ok(false),
        _ => Err(Error::Validation("invalid party flag".into())),
    }
}
fn check_voucher_view(node: Node<'_, '_>, allowed: &[&str]) -> Result<()> {
    for field in ["PERSISTEDVIEW", "OBJVIEW"] {
        let attribute = node
            .attributes()
            .find(|attribute| attribute.name().eq_ignore_ascii_case(field))
            .map(|attribute| attribute.value().trim());
        let value = direct_text(node, field);
        for view in attribute.into_iter().chain(value.as_deref()) {
            if !view.is_empty() && !allowed.iter().any(|name| view.eq_ignore_ascii_case(name)) {
                return Err(Error::Validation(format!("unsupported {field}")));
            }
        }
    }
    Ok(())
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
fn stock_opening_cost(node: Node<'_, '_>, quantity: i64, unit: &str) -> Result<Option<i64>> {
    let value = direct_text(node, "OPENINGVALUE")
        .filter(|value| !value.is_empty())
        .map(|value| xml_amount(&value))
        .transpose()?;
    let rate_text = direct_text(node, "OPENINGRATE").filter(|value| !value.is_empty());
    let rate = rate_text
        .as_deref()
        .map(|text| {
            let (number, suffix) = text
                .split_once('/')
                .map_or((text, None), |(n, u)| (n, Some(u)));
            if suffix.is_some_and(|suffix| normalized(suffix) != normalized(unit)) {
                return Err(Error::Validation(
                    "opening rate uses a different unit".into(),
                ));
            }
            let rate = Decimal::from_str(number.trim())
                .map_err(|_| Error::Validation("opening rate is invalid".into()))?;
            if rate < Decimal::ZERO || (!rate.is_zero() && rate.scale() > 5) {
                return Err(Error::Validation(
                    "opening rate must be non-negative with at most five decimal places".into(),
                ));
            }
            Ok(rate)
        })
        .transpose()?;
    if value.is_none_or(|value| value == 0) && rate.is_none_or(|rate| rate.is_zero()) {
        // Legacy files use zero fields for quantity-only openings.
        return Ok(None);
    }
    if quantity <= 0 {
        return Err(Error::Validation(
            "opening valuation requires a positive quantity".into(),
        ));
    }
    let nonzero_rate = rate.filter(|rate| !rate.is_zero());
    if value.is_some_and(|value| value < 0) && nonzero_rate.is_none() {
        return Err(Error::Validation(
            "a negative opening value needs a matching positive rate".into(),
        ));
    }
    let too_large = || Error::Validation("opening value is too large".into());
    let units = Decimal::new(quantity, 3);
    if let Some(value) = value {
        let value = value.checked_abs().ok_or_else(too_large)?;
        if let Some(rate) = nonzero_rate {
            let calculated = units.checked_mul(rate).ok_or_else(too_large)?;
            // A total may be more precise than the displayed rate. Accept only
            // amounts consistent with half a last-place rate unit plus rounding.
            let tolerance = Decimal::new(5, rate.scale() + 1)
                .checked_mul(units)
                .and_then(|value| value.checked_add(Decimal::new(5, 3)))
                .ok_or_else(too_large)?;
            if (Decimal::new(value, 2) - calculated).abs() > tolerance {
                return Err(Error::Validation(
                    "opening value conflicts with quantity and rate".into(),
                ));
            }
        }
        return Ok(Some(value));
    }
    let Some(rate) = nonzero_rate else {
        return Ok(None);
    };
    let minor = units
        .checked_mul(rate)
        .and_then(|value| value.checked_mul(Decimal::new(100, 0)))
        .ok_or_else(too_large)?
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_i64()
        .ok_or_else(too_large)?;
    Ok(Some(minor))
}

fn opening_cost_or_issue(
    node: Node<'_, '_>,
    quantity: i64,
    unit: &str,
    label: &str,
    issues: &mut Vec<String>,
) -> Option<i64> {
    match stock_opening_cost(node, quantity, unit) {
        Ok(cost) => cost,
        Err(error) => {
            issues.push(format!("{label}: {error}"));
            None
        }
    }
}

fn opening_location(
    requested: &str,
    known: &HashMap<String, Option<i64>>,
) -> Option<(String, Option<i64>)> {
    let name = if requested.trim().is_empty()
        || (requested.eq_ignore_ascii_case("Main Location") && !known.contains_key("main location"))
    {
        "Main"
    } else {
        requested
    };
    known
        .get(&normalized(name))
        .map(|id| (name.to_owned(), *id))
}

fn invoice_field(node: Node<'_, '_>, field: &str) -> Result<String> {
    direct_text(node, field)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Validation(format!("missing {field}")))
}
fn xml_credit_days(term: &str) -> Result<u32> {
    let parts: Vec<_> = term.split_whitespace().collect();
    if parts.len() != 2
        || !parts[0].bytes().all(|b| b.is_ascii_digit())
        || !(parts[1].eq_ignore_ascii_case("day") || parts[1].eq_ignore_ascii_case("days"))
    {
        return Err(Error::Validation(
            "credit period must be a whole number of days".into(),
        ));
    }
    parts[0]
        .parse::<u32>()
        .map_err(|_| Error::Validation("credit period is too large".into()))
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
fn xml_opening_bill(
    node: Node<'_, '_>,
    kind: InvoiceKind,
    opening_date: NaiveDate,
) -> Result<ImportedOpeningBill> {
    let reference = invoice_field(node, "NAME")?;
    let original_date = xml_date(&invoice_field(node, "BILLDATE")?)?;
    let original = parse_date(&original_date)?;
    if original > opening_date {
        return Err(Error::Validation(
            "Bill date follows the opening date".into(),
        ));
    }
    if direct_text(node, "BILLTYPE")
        .is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("New Ref"))
    {
        return Err(Error::Validation(
            "Opening bill must have a new reference".into(),
        ));
    }
    let amount_field = |field| -> Result<Option<i64>> {
        direct_text(node, field)
            .filter(|value| !value.is_empty())
            .as_deref()
            .map(xml_amount)
            .transpose()
    };
    let value = amount_field("OPENINGBALANCE")?;
    let amount = amount_field("AMOUNT")?;
    if value
        .zip(amount)
        .is_some_and(|(value, amount)| value != amount)
    {
        return Err(Error::Validation(
            "Opening bill amount fields disagree".into(),
        ));
    }
    let signed_amount = value
        .or(amount)
        .ok_or_else(|| Error::Validation("Opening bill amount is required".into()))?;
    let matching_side = match kind {
        InvoiceKind::Sale => signed_amount > 0,
        InvoiceKind::Purchase => signed_amount < 0,
    };
    if !matching_side {
        return Err(Error::Validation(
            "Opening bill amount has the wrong debit or credit side".into(),
        ));
    }
    let amount = signed_amount
        .checked_abs()
        .ok_or_else(|| Error::Validation("Opening bill amount is too large".into()))?;
    let credit_days = direct_text(node, "BILLCREDITPERIOD")
        .filter(|value| !value.is_empty())
        .as_deref()
        .map(xml_credit_days)
        .transpose()?;
    if credit_days.is_some_and(|days| {
        original
            .checked_add_days(chrono::Days::new(u64::from(days)))
            .is_none()
    }) {
        return Err(Error::Validation(
            "Opening bill credit period exceeds the supported date range".into(),
        ));
    }
    Ok(ImportedOpeningBill {
        kind,
        reference,
        original_date,
        credit_days,
        amount,
    })
}

fn opening_bills(
    node: Node<'_, '_>,
    ledger_amount: Option<i64>,
    category: Category,
    opening_date: Option<&str>,
    label: &str,
    issues: &mut Vec<String>,
) -> Vec<ImportedOpeningBill> {
    let nodes: Vec<_> = node
        .children()
        .filter(|child| {
            child.is_element()
                && local_name(*child).eq_ignore_ascii_case("BILLALLOCATIONS.LIST")
                && meaningful(*child)
        })
        .collect();
    let billwise = direct_text(node, "ISBILLWISEON").unwrap_or_default();
    if !billwise.is_empty()
        && !billwise.eq_ignore_ascii_case("yes")
        && !billwise.eq_ignore_ascii_case("no")
    {
        issues.push(format!("{label}: invalid bill-wise flag"));
    } else if billwise.eq_ignore_ascii_case("no") && !nodes.is_empty() {
        issues.push(format!(
            "{label}: bill-wise flag conflicts with opening bills"
        ));
    }
    if nodes.is_empty() {
        return Vec::new();
    }

    let kind = match (category, ledger_amount) {
        (Category::Asset, Some(amount)) if amount > 0 => InvoiceKind::Sale,
        (Category::Liability, Some(amount)) if amount < 0 => InvoiceKind::Purchase,
        _ => {
            issues.push(format!("{label}: opening bills need a debit opening balance for an asset or a credit opening balance for a liability"));
            return Vec::new();
        }
    };
    let Some(date) = opening_date.and_then(|date| parse_date(date).ok()) else {
        issues.push(format!("{label}: provide an opening date for the bills"));
        return Vec::new();
    };
    let mut bills = Vec::new();
    let mut references = HashSet::new();
    for node in nodes {
        invoice_list_fields(
            node,
            &[
                "NAME",
                "BILLDATE",
                "BILLTYPE",
                "OPENINGBALANCE",
                "AMOUNT",
                "BILLCREDITPERIOD",
            ],
            label,
            issues,
        );
        match xml_opening_bill(node, kind, date) {
            Ok(bill) if references.insert(normalized(&bill.reference)) => bills.push(bill),
            Ok(bill) => issues.push(format!(
                "{label}: duplicate opening bill reference {}",
                bill.reference
            )),
            Err(error) => issues.push(format!("{label}: opening bill: {error}")),
        }
    }
    let total: i128 = bills.iter().map(|bill| i128::from(bill.amount)).sum();
    if total != ledger_amount.map(i128::from).unwrap_or_default().abs() {
        issues.push(format!(
            "{label}: opening bill amounts must add up to the ledger opening balance"
        ));
    }
    bills
}

fn parse_imported_settlement(
    node: Node<'_, '_>,
    label: &str,
    metadata: SourceMetadata,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    issues: &mut Vec<String>,
) -> Result<ImportedSettlement> {
    check_fields(
        node,
        &[
            "DATE",
            "VOUCHERTYPENAME",
            "VOUCHERNUMBER",
            "NARRATION",
            "PARTYLEDGERNAME",
            "PERSISTEDVIEW",
            "OBJVIEW",
            "ISINVOICE",
            "LEDGERENTRIES.LIST",
            "ALLLEDGERENTRIES.LIST",
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
    check_voucher_view(node, &["Accounting Voucher View"])?;
    if direct_text(node, "ISINVOICE")
        .is_some_and(|v| !v.is_empty() && !v.eq_ignore_ascii_case("no"))
    {
        return Err(Error::Validation(
            "settlement must be an accounting voucher".into(),
        ));
    }
    let voucher_kind = parse_kind(&invoice_field(node, "VOUCHERTYPENAME")?)
        .ok_or_else(|| Error::Validation("unsupported settlement type".into()))?;
    let kind = match voucher_kind {
        VoucherKind::Receipt => InvoiceKind::Sale,
        VoucherKind::Payment => InvoiceKind::Purchase,
        _ => {
            return Err(Error::Validation(
                "bill allocations require a receipt or payment".into(),
            ))
        }
    };
    if let Some(attr) = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("VCHTYPE"))
    {
        if parse_kind(attr.value()) != Some(voucher_kind) {
            return Err(Error::Validation("conflicting settlement type".into()));
        }
    }
    let date = xml_date(&invoice_field(node, "DATE")?)?;
    if let Some(attr) = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("DATE"))
    {
        if xml_date(attr.value())? != date {
            return Err(Error::Validation("conflicting settlement date".into()));
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
    if entries.len() != 2 {
        return Err(Error::Validation(
            "settlement requires exactly two ledger entries".into(),
        ));
    }
    let mut party: Option<(String, Option<i64>, i64, ImportedBillUse)> = None;
    let mut bank: Option<(String, Option<i64>, i64)> = None;
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
        let (existing_id, category) = ledgers
            .get(&normalized(&ledger))
            .ok_or_else(|| Error::Validation(format!("ledger {ledger} is unavailable")))?;
        let amount = xml_amount(&invoice_field(entry, "AMOUNT")?)?;
        if amount == 0 {
            return Err(Error::Validation("zero ledger amount".into()));
        }
        invoice_side(entry, "ISDEEMEDPOSITIVE", amount > 0)?;
        invoice_side(entry, "ISLASTDEEMEDPOSITIVE", amount > 0)?;
        party_flag(entry)?;
        let bills: Vec<_> = entry
            .children()
            .filter(|n| {
                n.is_element()
                    && local_name(*n).eq_ignore_ascii_case("BILLALLOCATIONS.LIST")
                    && meaningful(*n)
            })
            .collect();
        if bills.is_empty() {
            if bank.is_some() || *category != Category::Asset {
                return Err(Error::Validation(
                    "settlement needs one cash or bank asset ledger".into(),
                ));
            }
            bank = Some((ledger, *existing_id, amount));
        } else {
            if party.is_some() || !matches!(category, Category::Asset | Category::Liability) {
                return Err(Error::Validation(
                    "settlement needs one party ledger".into(),
                ));
            }
            let mut allocations = Vec::new();
            let mut advance = None;
            let mut seen = HashSet::new();
            for bill in bills {
                invoice_list_fields(
                    bill,
                    &["NAME", "BILLTYPE", "AMOUNT", "BILLCREDITPERIOD"],
                    label,
                    issues,
                );
                let bill_type = normalized(&invoice_field(bill, "BILLTYPE")?);
                let bill_amount = xml_amount(&invoice_field(bill, "AMOUNT")?)?;
                if bill_amount == 0 || (bill_amount > 0) != (amount > 0) {
                    return Err(Error::Validation(
                        "bill allocation direction is inconsistent".into(),
                    ));
                }
                let value = bill_amount
                    .checked_abs()
                    .ok_or_else(|| Error::Validation("bill amount is too large".into()))?;
                let credit = direct_text(bill, "BILLCREDITPERIOD").filter(|v| !v.is_empty());
                if matches!(bill_type.as_str(), "advance" | "on account") {
                    if advance.is_some() {
                        return Err(Error::Validation(
                            "only one advance or on-account allocation per receipt or payment is supported".into(),
                        ));
                    }
                    if credit.is_some() {
                        return Err(Error::Validation(
                            "credit terms on advances are not supported".into(),
                        ));
                    }
                    let source = if bill_type == "advance" {
                        "Advance"
                    } else {
                        "On Account"
                    };
                    let reference = direct_text(bill, "NAME").filter(|value| !value.is_empty());
                    if bill_type == "advance" && reference.is_none() {
                        return Err(Error::Validation(
                            "an advance needs a bill reference".into(),
                        ));
                    }
                    advance = Some(ImportedAdvance {
                        amount: value,
                        source_bill: AdvanceSourceBill {
                            kind: source.into(),
                            reference,
                        },
                    });
                    continue;
                }
                if bill_type != "agst ref" {
                    return Err(Error::Validation("unsupported bill allocation type".into()));
                }
                let bill_reference = invoice_field(bill, "NAME")?;
                if !seen.insert(normalized(&bill_reference)) {
                    return Err(Error::Validation(
                        "bill reference occurs more than once".into(),
                    ));
                }
                allocations.push(ImportedBillAllocation {
                    reference: bill_reference,
                    amount: value,
                    existing_target: None,
                    expected_outstanding: 0,
                    credit_days: credit.map(|v| xml_credit_days(&v)).transpose()?,
                });
            }
            let sum = allocations
                .iter()
                .map(|v| i128::from(v.amount))
                .sum::<i128>()
                + advance.as_ref().map_or(0, |v| i128::from(v.amount));
            if sum != i128::from(amount).abs() {
                return Err(Error::Validation(
                    "bill allocations do not equal the party amount".into(),
                ));
            }
            let bills = ImportedBillUse {
                invoices: allocations,
                advance,
            };
            party = Some((ledger, *existing_id, amount, bills));
        }
    }
    let (party, existing_party_id, party_amount, bills) =
        party.ok_or_else(|| Error::Validation("bill allocations are missing".into()))?;
    let (bank, existing_bank_id, bank_amount) =
        bank.ok_or_else(|| Error::Validation("cash or bank ledger is missing".into()))?;
    if normalized(&party) == normalized(&bank)
        || i128::from(party_amount) + i128::from(bank_amount) != 0
        || (bank_amount > 0) != (kind == InvoiceKind::Sale)
    {
        return Err(Error::Validation(
            "party and bank amounts or directions are inconsistent".into(),
        ));
    }
    if direct_text(node, "PARTYLEDGERNAME")
        .is_some_and(|name| !name.is_empty() && normalized(&name) != normalized(&party))
    {
        return Err(Error::Validation(
            "party ledger differs from the allocated ledger".into(),
        ));
    }
    Ok(ImportedSettlement {
        kind,
        date,
        reference,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
        party,
        bank,
        existing_bank_id,
        existing_party_id,
        bills,
        metadata,
    })
}
/// Parse only invoices whose party, accounting, amounts and bill reference
/// can all be represented without dropping information.
fn parse_imported_invoice(
    node: Node<'_, '_>,
    label: &str,
    metadata: SourceMetadata,
    entry_mode: InvoiceEntryMode,
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
            "PARTYLEDGERNAME",
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
    let mut inventory_rows: Vec<_> = node
        .children()
        .filter(|child| {
            child.is_element()
                && local_name(*child).eq_ignore_ascii_case("ALLINVENTORYENTRIES.LIST")
                && meaningful(*child)
        })
        .map(|child| (child, None))
        .collect();
    let has_header_inventory = !inventory_rows.is_empty();
    let allowed_views: &[&str] = match entry_mode {
        InvoiceEntryMode::Voucher => &["Accounting Voucher View"],
        InvoiceEntryMode::Invoice if has_header_inventory => &["Invoice Voucher View"],
        InvoiceEntryMode::Invoice => &["Invoice Voucher View", "Accounting Voucher View"],
    };
    check_voucher_view(node, allowed_views)?;
    let voucher_kind = parse_kind(&invoice_field(node, "VOUCHERTYPENAME")?)
        .ok_or_else(|| Error::Validation("unsupported invoice type".into()))?;
    let kind = match voucher_kind {
        VoucherKind::Sales => InvoiceKind::Sale,
        VoucherKind::Purchase => InvoiceKind::Purchase,
        _ => {
            return Err(Error::Validation(
                "only sales and purchase invoices are supported".into(),
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
    let mut party = None;
    let header_party = direct_text(node, "PARTYLEDGERNAME").filter(|name| !name.is_empty());
    let mut adjustments = Vec::new();
    let mut original_postings = Vec::new();
    struct StockLedgerBreakdown {
        ledger: String,
        posting_amount: i64,
        stock_amount: i128,
    }
    let mut breakdowns = Vec::<StockLedgerBreakdown>::new();
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
                "INVENTORYALLOCATIONS.LIST",
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
        let named_party = header_party
            .as_ref()
            .is_some_and(|name| normalized(name) == normalized(&ledger));
        let is_party = party_flag(entry)?
            || (named_party
                && direct_text(entry, "ISPARTYLEDGER").is_none_or(|value| value.is_empty()));
        if entry_mode == InvoiceEntryMode::Voucher {
            original_postings.push(Line {
                ledger: ledger.clone(),
                amount,
                is_party,
            });
        }
        let stock: Vec<_> = entry
            .children()
            .filter(|child| {
                child.is_element()
                    && local_name(*child).eq_ignore_ascii_case("INVENTORYALLOCATIONS.LIST")
                    && meaningful(*child)
            })
            .collect();
        let has_stock = !stock.is_empty();
        if has_stock {
            if entry_mode != InvoiceEntryMode::Voucher || is_party || has_header_inventory {
                return Err(Error::Validation(
                    "Ledger inventory allocations must belong to an unmarked trade posting in voucher mode".into(),
                ));
            }
            let breakdown = breakdowns.len();
            inventory_rows.extend(stock.into_iter().map(|row| (row, Some(breakdown))));
            breakdowns.push(StockLedgerBreakdown {
                ledger: ledger.clone(),
                posting_amount: amount,
                stock_amount: 0,
            });
        }
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
            if party.is_some() || (amount > 0) != party_debit || bills.is_empty() {
                return Err(Error::Validation(
                    "The document needs one party and bill allocations with the correct direction"
                        .into(),
                ));
            }
            let mut schedule = Vec::with_capacity(bills.len());
            let mut advance_applications = Vec::new();
            let mut advance_references = HashSet::new();
            let mut signed_total = 0_i128;
            for bill in bills {
                invoice_list_fields(
                    bill,
                    &["NAME", "BILLTYPE", "AMOUNT", "BILLCREDITPERIOD"],
                    label,
                    issues,
                );
                let reference = invoice_field(bill, "NAME")?;
                let bill_amount = xml_amount(&invoice_field(bill, "AMOUNT")?)?;
                if bill_amount == 0 || (bill_amount > 0) != party_debit {
                    return Err(Error::Validation(
                        "Bill allocation must follow the party posting's direction".into(),
                    ));
                }
                let amount = bill_amount
                    .checked_abs()
                    .ok_or_else(|| Error::Validation("Bill amount is too large".into()))?;
                signed_total += i128::from(bill_amount);
                let credit =
                    direct_text(bill, "BILLCREDITPERIOD").filter(|value| !value.is_empty());
                match normalized(&invoice_field(bill, "BILLTYPE")?).as_str() {
                    "agst ref" => {
                        if credit.is_some() {
                            return Err(Error::Validation(
                                "An application of an advance cannot carry credit terms".into(),
                            ));
                        }
                        if !advance_references.insert(normalized(&reference)) {
                            return Err(Error::Validation(format!(
                                "Advance reference {reference} occurs more than once"
                            )));
                        }
                        advance_applications.push(ImportedAdvanceApplication {
                            reference,
                            amount,
                            existing_advance_id: None,
                            expected_available: 0,
                        });
                        continue;
                    }
                    "new ref" => {}
                    _ => {
                        return Err(Error::Validation(
                            "Unsupported invoice bill allocation type".into(),
                        ))
                    }
                }
                let (due_date, credit_days) = match credit {
                    Some(value) => match xml_date(&value).or_else(|_| {
                        NaiveDate::parse_from_str(&value, "%d-%b-%Y")
                            .map(|date| date.to_string())
                            .map_err(|_| Error::Validation("Invalid bill due date".into()))
                    }) {
                        Ok(date) => (Some(date), None),
                        Err(_) => (None, Some(xml_credit_days(&value)?)),
                    },
                    None => (None, None),
                };
                if let Some(days) = credit_days {
                    parse_date(&date)?
                        .checked_add_days(chrono::Days::new(u64::from(days)))
                        .ok_or_else(|| {
                            Error::Validation("Credit period produces an invalid due date".into())
                        })?;
                }
                schedule.push(BillSplitDraft {
                    reference,
                    amount,
                    due_date,
                    credit_days,
                });
            }
            if signed_total != i128::from(amount) {
                return Err(Error::Validation(
                    "Bill allocations must add up to the full party amount".into(),
                ));
            }
            party = Some((ledger, amount, schedule, advance_applications));
        } else {
            if !bills.is_empty() {
                return Err(Error::Validation(
                    "additional ledger bill allocations are not supported".into(),
                ));
            }
            if !has_stock {
                adjustments.push(ImportedInvoiceAdjustment {
                    ledger,
                    existing_ledger_id: *existing_ledger_id,
                    amount: if party_debit { -amount } else { amount },
                });
            }
        }
    }
    let (party_name, party_amount, bills, advance_applications) =
        party.ok_or_else(|| Error::Validation("party ledger is missing".into()))?;
    if header_party.is_some_and(|name| normalized(&name) != normalized(&party_name)) {
        return Err(Error::Validation(
            "Voucher party differs from its bill ledger".into(),
        ));
    }
    let mut lines = Vec::new();
    let mut trade: Option<String> = None;
    let mut subtotal = 0_i128;
    for (item_node, posting) in inventory_rows {
        invoice_list_fields(
            item_node,
            &[
                "STOCKITEMNAME",
                "ISDEEMEDPOSITIVE",
                "RATE",
                "DISCOUNT",
                "DISCOUNTAMOUNT",
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
        let source_amount = xml_amount(&invoice_field(item_node, "AMOUNT")?)?;
        if source_amount != 0 && (source_amount > 0) == party_debit {
            return Err(Error::Validation(
                "item amount direction is inconsistent".into(),
            ));
        }
        let percentage = direct_text(item_node, "DISCOUNT").filter(|v| !v.is_empty());
        let fixed = direct_text(item_node, "DISCOUNTAMOUNT")
            .filter(|v| !v.is_empty())
            .map(|v| parse_amount(&v))
            .transpose()?;
        let percentage_is_nonzero = if let Some(value) = &percentage {
            let rate = value
                .parse::<rust_decimal::Decimal>()
                .map_err(|_| Error::Validation("invalid item discount percentage".into()))?;
            !rate.is_zero()
        } else {
            false
        };
        if percentage_is_nonzero && fixed.is_some_and(|value| value != 0) {
            return Err(Error::Validation(
                "combined item discount fields are not supported".into(),
            ));
        }
        let discount = if percentage_is_nonzero {
            InvoiceDiscount::Percent(percentage.unwrap_or_default())
        } else if let Some(amount) = fixed.filter(|value| *value != 0) {
            InvoiceDiscount::Fixed(amount)
        } else {
            InvoiceDiscount::None
        };
        let unit_price =
            if let Some(rate) = direct_text(item_node, "RATE").filter(|v| !v.is_empty()) {
                let (price, rate_unit) = rate
                    .split_once('/')
                    .ok_or_else(|| Error::Validation("rate must specify the base unit".into()))?;
                if normalized(rate_unit) != normalized(unit) {
                    return Err(Error::Validation("rate uses another unit".into()));
                }
                parse_amount(price)?
            } else {
                if posting.is_none() || !matches!(discount, InvoiceDiscount::None) {
                    return Err(Error::Validation("item rate is missing".into()));
                }
                let numerator = i128::from(source_amount).abs() * 1000;
                let quantity = i128::from(quantity);
                if numerator % quantity != 0 {
                    return Err(Error::Validation(
                        "An omitted item rate cannot be derived exactly in hundredths".into(),
                    ));
                }
                i64::try_from(numerator / quantity)
                    .map_err(|_| Error::Validation("item rate is too large".into()))?
            };
        let (amount, _) = discounted_line_amount(quantity, unit_price, &discount)?;
        let expected = if party_debit { -amount } else { amount };
        if source_amount != expected {
            return Err(Error::Validation(
                "item amount differs from quantity and rate".into(),
            ));
        }
        invoice_side(item_node, "ISDEEMEDPOSITIVE", !party_debit)?;
        let allocations: Vec<_> = item_node
            .children()
            .filter(|n| {
                n.is_element() && local_name(*n).eq_ignore_ascii_case("ACCOUNTINGALLOCATIONS.LIST")
            })
            .collect();
        let ledger = if let Some(index) = posting {
            if allocations.iter().any(|allocation| meaningful(*allocation)) {
                return Err(Error::Validation(
                    "Stock on a ledger posting cannot also have an item accounting allocation"
                        .into(),
                ));
            }
            let breakdown = &mut breakdowns[index];
            breakdown.stock_amount += i128::from(expected);
            breakdown.ledger.clone()
        } else {
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
            if xml_amount(&invoice_field(allocation, "AMOUNT")?)? != expected {
                return Err(Error::Validation(
                    "item accounting allocation differs from the item amount".into(),
                ));
            }
            invoice_side(allocation, "ISDEEMEDPOSITIVE", !party_debit)?;
            invoice_field(allocation, "LEDGERNAME")?
        };
        let (existing_trade_ledger_id, category) = ledgers
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
        let locations_in_line: Vec<_> = item_node
            .children()
            .filter(|n| {
                n.is_element()
                    && local_name(*n).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
                    && meaningful(*n)
            })
            .collect();
        let mut stock_allocations = Vec::new();
        if locations_in_line.is_empty() {
            let location = "Main".to_owned();
            let existing_location_id = *locations
                .get(&normalized(&location))
                .ok_or_else(|| Error::Validation("default stock location is unavailable".into()))?;
            stock_allocations.push(ImportedStockAllocation {
                location,
                existing_location_id,
                batch: None,
                existing_batch_id: None,
                quantity,
            });
        } else {
            if locations_in_line.len() > 1 && matches!(discount, InvoiceDiscount::Fixed(_)) {
                return Err(Error::Validation(
                    "fixed discounts across multiple stock allocations are not supported".into(),
                ));
            }
            let mut total_quantity = 0_i128;
            let mut total_amount = 0_i128;
            for location_node in locations_in_line {
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
                let allocated_actual =
                    xml_stock_quantity(&invoice_field(location_node, "ACTUALQTY")?, unit)?;
                let allocated_billed =
                    xml_stock_quantity(&invoice_field(location_node, "BILLEDQTY")?, unit)?;
                if allocated_actual == 0
                    || allocated_actual != allocated_billed
                    || (allocated_actual > 0) != (actual > 0)
                {
                    return Err(Error::Validation(
                        "location quantities do not match the item direction".into(),
                    ));
                }
                let allocated_quantity = allocated_actual
                    .checked_abs()
                    .ok_or_else(|| Error::Validation("quantity is too large".into()))?;
                if allocated_quantity % 10_i64.pow(u32::from(3 - *precision)) != 0 {
                    return Err(Error::Validation(
                        "allocation exceeds unit precision".into(),
                    ));
                }
                let (allocated_amount, _) =
                    discounted_line_amount(allocated_quantity, unit_price, &discount)?;
                let expected_amount = if party_debit {
                    -allocated_amount
                } else {
                    allocated_amount
                };
                if xml_amount(&invoice_field(location_node, "AMOUNT")?)? != expected_amount {
                    return Err(Error::Validation(
                        "allocation amount differs from quantity and rate".into(),
                    ));
                }
                let location = invoice_field(location_node, "GODOWNNAME")?;
                let existing_location_id =
                    *locations.get(&normalized(&location)).ok_or_else(|| {
                        Error::Validation(format!("stock location {location} is unavailable"))
                    })?;
                let batch =
                    direct_text(location_node, "BATCHNAME").filter(|value| !value.is_empty());
                let existing_batch_id = batch.as_ref().and_then(|batch| {
                    batches
                        .get(&(normalized(&name), normalized(batch)))
                        .copied()
                });
                total_quantity += i128::from(allocated_quantity);
                total_amount += i128::from(allocated_amount);
                stock_allocations.push(ImportedStockAllocation {
                    location,
                    existing_location_id,
                    batch,
                    existing_batch_id,
                    quantity: allocated_quantity,
                });
            }
            if total_quantity != i128::from(quantity) || total_amount != i128::from(amount) {
                return Err(Error::Validation(
                    "location allocations do not match the item totals".into(),
                ));
            }
        }
        if trade.is_none() {
            trade = Some(ledger.clone());
        }
        subtotal += i128::from(amount);
        lines.push(ImportedInvoiceLine {
            item: name,
            existing_item_id: *existing_item_id,
            unit: unit.clone(),
            precision: *precision,
            allocations: stock_allocations,
            existing_trade_ledger_id: *existing_trade_ledger_id,
            trade_ledger: ledger,
            quantity,
            unit_price,
            discount,
        });
    }
    for breakdown in breakdowns {
        if breakdown.stock_amount != i128::from(breakdown.posting_amount) {
            return Err(Error::Validation(format!(
                "Stock amounts do not reconcile to the posting for {}",
                breakdown.ledger
            )));
        }
    }
    let trade = match trade {
        Some(trade) => trade,
        None => adjustments
            .iter()
            .find(|adjustment| {
                if adjustment.amount <= 0 {
                    return false;
                }
                let category = ledgers
                    .get(&normalized(&adjustment.ledger))
                    .map(|entry| entry.1);
                (kind == InvoiceKind::Sale && category == Some(Category::Income))
                    || (kind == InvoiceKind::Purchase
                        && matches!(category, Some(Category::Expense | Category::Asset)))
            })
            .map(|adjustment| adjustment.ledger.clone())
            .ok_or_else(|| {
                Error::Validation(
                    "a non-stock invoice needs a positive sales or purchase ledger line".into(),
                )
            })?,
    };
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
    if lines
        .iter()
        .any(|line| normalized(&line.trade_ledger) == normalized(&party_name))
    {
        return Err(Error::Validation(
            "item ledger must differ from the party".into(),
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
        entry_mode,
        date,
        reference,
        bills,
        advance_applications,
        advance_bill_reference: None,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
        party: party_name,
        existing_party_id,
        trade,
        existing_trade_id,
        lines,
        adjustments,
        original_postings,
        metadata,
    })
}
type ReferenceKey = (String, String, String);

struct NamedAdvance {
    id: Option<i64>,
    reference: String,
    date: NaiveDate,
    latest_release: Option<NaiveDate>,
    available: i128,
}

type NamedAdvances = HashMap<ReferenceKey, Option<NamedAdvance>>;

fn add_named_advance(
    advances: &mut NamedAdvances,
    kind: InvoiceKind,
    party: &str,
    advance: NamedAdvance,
) {
    let key = (
        normalized(kind.as_str()),
        normalized(party),
        normalized(&advance.reference),
    );
    advances
        .entry(key)
        .and_modify(|row| *row = None)
        .or_insert(Some(advance));
}

fn read_named_advances(conn: &Connection, company: i64) -> Result<NamedAdvances> {
    let mut advances = HashMap::new();
    for row in read_party_advances(conn, company, None)? {
        let reference = match row.source_bill {
            Some(source) if source.kind == "Advance" => source.reference,
            Some(_) => None,
            None => Some(row.reference),
        };
        let Some(reference) = reference else { continue };
        add_named_advance(
            &mut advances,
            row.kind,
            &row.party,
            NamedAdvance {
                id: Some(row.id),
                reference,
                date: parse_date(&row.date)?,
                latest_release: advance_allocation_state(conn, company, row.id, None)?
                    .latest_release,
                available: row.available,
            },
        );
    }
    Ok(advances)
}

fn named_advance<'a>(
    advances: &'a mut NamedAdvances,
    kind: InvoiceKind,
    party: &str,
    date: NaiveDate,
    application: &ImportedAdvanceApplication,
) -> Result<&'a mut NamedAdvance> {
    let reference = &application.reference;
    let key = (
        normalized(kind.as_str()),
        normalized(party),
        normalized(reference),
    );
    let advance = advances
        .get_mut(&key)
        .ok_or_else(|| {
            Error::Validation(format!(
                "Named advance {reference} is unavailable for this party and type"
            ))
        })?
        .as_mut()
        .ok_or_else(|| Error::Validation(format!("Named advance {reference} is ambiguous")))?;
    if &advance.reference != reference {
        return Err(Error::Validation(format!(
            "Advance reference {reference} differs from the saved spelling"
        )));
    }
    if date < advance.date || advance.latest_release.is_some_and(|release| date < release) {
        return Err(Error::Validation(format!(
            "Application precedes advance {reference} or its latest release"
        )));
    }
    if i128::from(application.amount) > advance.available {
        return Err(Error::Validation(format!(
            "Application exceeds the available amount on advance {reference}"
        )));
    }
    Ok(advance)
}

fn preview_advance_applications(
    conn: &Connection,
    plan: &mut ImportPlan,
    bill_references: &mut HashSet<ReferenceKey>,
) -> Result<()> {
    let mut advances = read_named_advances(conn, plan.company_id)?;
    for settlement in &plan.settlements {
        if let Some(ImportedAdvance {
            amount,
            source_bill,
        }) = &settlement.bills.advance
        {
            if let Some(reference) = source_bill
                .reference
                .as_ref()
                .filter(|_| source_bill.kind == "Advance")
            {
                add_named_advance(
                    &mut advances,
                    settlement.kind,
                    &settlement.party,
                    NamedAdvance {
                        id: None,
                        reference: reference.clone(),
                        date: parse_date(&settlement.date)?,
                        latest_release: None,
                        available: (*amount).into(),
                    },
                );
            }
        }
    }
    for invoice in plan
        .invoices
        .iter_mut()
        .filter(|invoice| !invoice.advance_applications.is_empty())
    {
        let date = parse_date(&invoice.date)?;
        let mut funded = 0_i64;
        for application in &mut invoice.advance_applications {
            funded = funded
                .checked_add(application.amount)
                .ok_or_else(|| Error::Validation("Invoice advance total is too large".into()))?;
            match named_advance(
                &mut advances,
                invoice.kind,
                &invoice.party,
                date,
                application,
            ) {
                Ok(advance) => {
                    application.existing_advance_id = advance.id;
                    application.expected_available = advance.available;
                    advance.available -= i128::from(application.amount);
                }
                Err(error) => plan
                    .issues
                    .push(format!("Voucher {}: {error}", invoice.reference)),
            }
        }
        // The original advance reference can fund several invoices. Reserve a
        // distinct local bill so the exported unpaid references keep their amounts.
        let prefix = format!("{} (advance)", invoice.reference);
        let kind = normalized(invoice.kind.as_str());
        let party = normalized(&invoice.party);
        let mut reference = prefix.clone();
        let mut suffix = 2;
        while !bill_references.insert((kind.clone(), party.clone(), normalized(&reference))) {
            reference = format!("{prefix} {suffix}");
            suffix += 1;
        }
        invoice.bills.push(BillSplitDraft {
            reference: reference.clone(),
            amount: funded,
            due_date: None,
            credit_days: None,
        });
        invoice.advance_bill_reference = Some(reference);
    }
    Ok(())
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
        self.preview_xml_import_batch(company_id, &[bytes], opening_date)
    }

    /// Resolve masters and transactions across independent XML documents in one plan.
    pub fn preview_xml_import_batch(
        &self,
        company_id: i64,
        files: &[&[u8]],
        opening_date: Option<&str>,
    ) -> Result<ImportPlan> {
        if !self.companies()?.iter().any(|c| c.id == company_id) {
            return Err(Error::Validation("Select an existing company".into()));
        }
        if files.is_empty() {
            return Err(Error::Validation("Select at least one XML file".into()));
        }
        if files
            .iter()
            .try_fold(MAX_XML_IMPORT_BYTES, |remaining, file| {
                remaining.checked_sub(file.len())
            })
            .is_none()
        {
            return Err(Error::Validation(
                "The XML files exceed the combined 50 MiB import limit".into(),
            ));
        }
        let mut texts = Vec::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            let text = decode(file).map_err(|e| {
                Error::Validation(format!("Cannot read XML file {}: {e}", index + 1))
            })?;
            texts.push(text);
        }
        let mut remaining_nodes = MAX_XML_NODES;
        let mut documents = Vec::with_capacity(texts.len());
        for (index, text) in texts.iter().enumerate() {
            let document = Document::parse_with_options(
                text,
                ParsingOptions {
                    nodes_limit: remaining_nodes,
                    ..ParsingOptions::default()
                },
            )
            .map_err(|e| Error::Validation(format!("Cannot read XML file {}: {e}", index + 1)))?;
            remaining_nodes -= document.descendants().len() as u32;
            documents.push(document);
        }
        let nodes = || documents.iter().flat_map(Document::descendants);
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
            settlements: Vec::new(),
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
        for node in nodes().filter(|n| {
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
        for node in
            nodes().filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("GROUP"))
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
        for node in nodes().filter(|n| {
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
                    "ISBILLWISEON",
                    "BILLCREDITPERIOD",
                    "BILLALLOCATIONS.LIST",
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
            if crate::opening::is_control_name(&name) {
                plan.issues.push(format!(
                    "Ledger {name}: name is reserved for opening balances"
                ));
            }
            let opening_bills = opening_bills(
                node,
                opening,
                category,
                plan.opening_date.as_deref(),
                &label,
                &mut plan.issues,
            );
            let credit_days = direct_text(node, "BILLCREDITPERIOD")
                .filter(|value| !value.is_empty())
                .and_then(|value| {
                    if !matches!(category, Category::Asset | Category::Liability) {
                        plan.issues.push(format!(
                            "{label}: a credit period requires an asset or liability ledger"
                        ));
                        return None;
                    }
                    match xml_credit_days(&value) {
                        Ok(days) => Some(days),
                        Err(error) => {
                            plan.issues.push(format!("{label}: {error}"));
                            None
                        }
                    }
                });
            plan.accounts.push(Account {
                name,
                group,
                category,
                profile,
                opening,
                opening_bills,
                credit_days,
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
        for node in
            nodes().filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("UNIT"))
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
        for node in
            nodes().filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("STOCKGROUP"))
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
        let locations_by_id: HashMap<_, _> = existing_locations
            .iter()
            .map(|location| (location.id, location))
            .collect();
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
        let mut pending_locations = Vec::new();
        for node in nodes().filter(|n| {
            n.is_element()
                && local_name(*n).eq_ignore_ascii_case("GODOWN")
                && !n.ancestors().any(|a| {
                    a.is_element()
                        && ["VOUCHER", "STOCKITEM"]
                            .iter()
                            .any(|tag| local_name(a).eq_ignore_ascii_case(tag))
                })
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
            if normalized(&name) == "primary" {
                plan.issues
                    .push(format!("{label}: the root location is implicit"));
                continue;
            }
            let parent = direct_text(node, "PARENT")
                .filter(|name| !name.is_empty() && normalized(name) != "primary");
            let metadata = source_metadata(node, &label, &mut plan.issues);
            register_metadata(
                "stocklocation",
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            if let Some(existing) = known_locations
                .get(&normalized(&name))
                .copied()
                .flatten()
                .and_then(|id| locations_by_id.get(&id))
            {
                let actual_parent = existing
                    .parent_id
                    .and_then(|id| locations_by_id.get(&id))
                    .map(|location| normalized(&location.name));
                if actual_parent != parent.as_deref().map(normalized) {
                    plan.issues
                        .push(format!("{label}: existing parent differs"));
                }
                if !metadata.is_empty() {
                    plan.existing_location_metadata
                        .push((existing.id, metadata));
                }
            } else {
                pending_locations.push(StockLocationImport {
                    name,
                    parent,
                    metadata,
                });
            }
        }
        let mut ready = VecDeque::new();
        let mut waiting = BTreeMap::<String, Vec<StockLocationImport>>::new();
        for location in pending_locations {
            match location.parent.as_deref().map(normalized) {
                Some(parent) if !known_locations.contains_key(&parent) => {
                    waiting.entry(parent).or_default().push(location);
                }
                _ => ready.push_back(location),
            }
        }
        while let Some(location) = ready.pop_front() {
            let name = normalized(&location.name);
            if let Some(children) = waiting.remove(&name) {
                ready.extend(children);
            }
            known_locations.insert(name, None);
            plan.locations.push(location);
        }
        for location in waiting.into_values().flatten() {
            plan.issues.push(format!(
                "Stock location {}: parent is missing or cyclic",
                location.name
            ));
        }
        let mut known_items = HashSet::<String>::new();
        for item in self.stock_items(company_id)? {
            if !known_items.insert(normalized(&item.name)) {
                plan.issues
                    .push(format!("Stock item {} is ambiguous", item.name));
            }
        }
        for node in
            nodes().filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("STOCKITEM"))
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
                    "GODOWN",
                    "GODOWNNAME",
                    "BATCHNAME",
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
            let top_cost = opening_cost_or_issue(
                node,
                top_opening.unwrap_or(0),
                &unit,
                &label,
                &mut plan.issues,
            );
            let top_location = direct_text(node, "GODOWN").filter(|value| !value.is_empty());
            let location_name = direct_text(node, "GODOWNNAME").filter(|value| !value.is_empty());
            if top_location
                .as_ref()
                .zip(location_name.as_ref())
                .is_some_and(|(a, b)| normalized(a) != normalized(b))
            {
                plan.issues
                    .push(format!("{label}: opening locations conflict"));
            }
            let top_location = top_location.or(location_name);
            let top_batch = direct_text(node, "BATCHNAME").filter(|value| !value.is_empty());
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
                let batch = second.or(first).or_else(|| top_batch.clone());
                if top_batch
                    .as_ref()
                    .zip(batch.as_ref())
                    .is_some_and(|(a, b)| normalized(a) != normalized(b))
                {
                    plan.issues
                        .push(format!("{label}: item and allocation batch names conflict"));
                }
                let requested_location = direct_text(*allocation, "GODOWNNAME")
                    .filter(|v| !v.is_empty())
                    .or_else(|| top_location.clone())
                    .unwrap_or_default();
                let (location, existing_location_id) =
                    match opening_location(&requested_location, &known_locations) {
                        Some(location) => location,
                        None => {
                            plan.issues.push(format!(
                                "{label}: stock location {requested_location} is unavailable"
                            ));
                            continue;
                        }
                    };
                if let Some(expected) = &top_location {
                    if !opening_location(expected, &known_locations)
                        .is_some_and(|(name, _)| normalized(&name) == normalized(&location))
                    {
                        plan.issues
                            .push(format!("{label}: item and allocation locations conflict"));
                    }
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
                let allocation_label = format!("{label}, opening at {location}");
                let cost = opening_cost_or_issue(
                    *allocation,
                    quantity,
                    &unit,
                    &allocation_label,
                    &mut plan.issues,
                );
                openings.push(StockOpening {
                    location,
                    existing_location_id,
                    batch,
                    manufactured_on: manufactured,
                    quantity,
                    cost,
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
                    let request = top_location.as_deref().unwrap_or("Main");
                    let (location, existing_location_id) =
                        opening_location(request, &known_locations).unwrap_or_else(|| {
                            plan.issues
                                .push(format!("{label}: stock location {request} is unavailable"));
                            (request.to_owned(), None)
                        });
                    openings.push(StockOpening {
                        location,
                        existing_location_id,
                        batch: top_batch,
                        manufactured_on: None,
                        quantity,
                        cost: top_cost,
                    });
                } else if top_location.is_some() || top_batch.is_some() {
                    plan.issues.push(format!(
                        "{label}: a location or batch requires an opening quantity"
                    ));
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
                if let Some(cost) = top_cost {
                    let costed: i128 = openings
                        .iter()
                        .filter_map(|opening| opening.cost)
                        .map(i128::from)
                        .sum();
                    let mut missing = openings
                        .iter_mut()
                        .filter(|opening| opening.quantity != 0 && opening.cost.is_none());
                    match (missing.next(), missing.next()) {
                        (Some(opening), None)
                            if opening.quantity > 0 && costed <= i128::from(cost) =>
                        {
                            // Exactly one allocation remains, so its residual cost is determined.
                            opening.cost = Some(cost - costed as i64);
                        }
                        (None, None) if costed == i128::from(cost) => {}
                        _ => plan.issues.push(format!(
                            "{label}: batch opening costs do not reconcile to the item value"
                        )),
                    }
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
        let mut bill_references = HashSet::<(String, String, String)>::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT i.kind, l.name, v.reference, COALESCE(b.reference, i.bill_reference, v.reference)
                 FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
                 LEFT JOIN invoice_bill_splits b ON b.invoice_id = i.id
                 JOIN ledgers l ON l.id = i.party_ledger_id WHERE i.company_id = ?1",
            )?;
            for row in stmt.query_map([company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })? {
                let (kind, party, reference, bill_reference) = row?;
                let kind = normalized(&kind);
                let party = normalized(&party);
                invoice_references.insert((kind.clone(), party.clone(), normalized(&reference)));
                bill_references.insert((kind, party, normalized(&bill_reference)));
            }
        }
        let mut pending_settlements = Vec::new();
        for account in &plan.accounts {
            for bill in &account.opening_bills {
                let key = (
                    normalized(bill.kind.as_str()),
                    normalized(&account.name),
                    normalized(&bill.reference),
                );
                let new_document = invoice_references.insert(key.clone());
                let new_bill = bill_references.insert(key);
                if !new_document || !new_bill {
                    plan.issues.push(format!(
                        "Ledger {}: opening bill reference {} already exists",
                        account.name, bill.reference
                    ));
                }
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
        for node in
            nodes().filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("VOUCHER"))
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
            let has_bill_details = node
                .children()
                .filter(|entry| {
                    entry.is_element()
                        && ["LEDGERENTRIES.LIST", "ALLLEDGERENTRIES.LIST"]
                            .iter()
                            .any(|tag| local_name(*entry).eq_ignore_ascii_case(tag))
                })
                .any(|entry| {
                    entry.children().any(|bill| {
                        bill.is_element()
                            && local_name(bill).eq_ignore_ascii_case("BILLALLOCATIONS.LIST")
                            && meaningful(bill)
                    })
                });
            let kind = direct_text(node, "VOUCHERTYPENAME").and_then(|value| parse_kind(&value));
            let invoice_flag = direct_text(node, "ISINVOICE");
            let entry_mode = if invoice_flag
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("yes"))
            {
                Some(InvoiceEntryMode::Invoice)
            } else if has_bill_details
                && matches!(kind, Some(VoucherKind::Sales | VoucherKind::Purchase))
                && invoice_flag
                    .as_deref()
                    .is_none_or(|value| value.is_empty() || value.eq_ignore_ascii_case("no"))
            {
                Some(InvoiceEntryMode::Voucher)
            } else {
                None
            };
            if let Some(entry_mode) = entry_mode {
                match parse_imported_invoice(
                    node,
                    &label,
                    metadata,
                    entry_mode,
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
                        for bill in &invoice.bills {
                            if !bill_references.insert((
                                normalized(invoice.kind.as_str()),
                                normalized(&invoice.party),
                                normalized(&bill.reference),
                            )) {
                                plan.issues.push(format!(
                                    "{label}: bill reference {} already exists for this party and type",
                                    bill.reference
                                ));
                            }
                        }
                        plan.invoices.push(invoice);
                    }
                    Err(e) => plan.issues.push(format!("{label}: {e}")),
                }
                continue;
            }
            if has_bill_details {
                pending_settlements.push((node, label, metadata));
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
                    "OBJVIEW",
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
            if let Err(error) = check_voucher_view(node, &["Accounting Voucher View"]) {
                plan.issues.push(format!("{label}: {error}"));
            }
            let kind = match kind {
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
                let is_party = match party_flag(entry) {
                    Ok(is_party) => is_party,
                    Err(error) => {
                        plan.issues.push(format!("{label}: {error}"));
                        continue;
                    }
                };
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
                lines.push(Line {
                    ledger,
                    amount,
                    is_party,
                });
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
        if !pending_settlements.is_empty() {
            let mut advance_references = self
                .party_advances(company_id, None)?
                .into_iter()
                .map(|row| {
                    (
                        normalized(row.kind.as_str()),
                        normalized(&row.party),
                        normalized(&row.reference),
                    )
                })
                .collect::<HashSet<_>>();
            for (node, label, metadata) in pending_settlements {
                match parse_imported_settlement(
                    node,
                    &label,
                    metadata,
                    &invoice_ledgers,
                    &mut plan.issues,
                ) {
                    Ok(settlement) => {
                        let voucher_kind = if settlement.kind == InvoiceKind::Sale {
                            VoucherKind::Receipt
                        } else {
                            VoucherKind::Payment
                        };
                        if !references.insert((
                            normalized(voucher_kind.as_str()),
                            settlement.date.clone(),
                            normalized(&settlement.reference),
                        )) {
                            plan.issues.push(format!(
                            "{label}: a voucher with the same type, date and number already exists"
                        ));
                        }
                        if settlement.bills.advance.is_some()
                            && !advance_references.insert((
                                normalized(settlement.kind.as_str()),
                                normalized(&settlement.party),
                                normalized(&settlement.reference),
                            ))
                        {
                            plan.issues.push(format!("{label}: an advance with this type, party and reference already exists"));
                        }
                        plan.settlements.push(settlement);
                    }
                    Err(error) => plan.issues.push(format!("{label}: {error}")),
                }
            }
        }
        let has_advance_applications = plan
            .invoices
            .iter()
            .any(|invoice| !invoice.advance_applications.is_empty());
        if has_advance_applications {
            preview_advance_applications(&self.conn, &mut plan, &mut bill_references)?;
        }
        if plan.settlement_count() != 0 {
            struct PreviewBill {
                target: Option<BillSettlementTarget>,
                invoice_key: ReferenceKey,
                invoice_date: String,
                reference: String,
                outstanding: i128,
                latest_reversal: Option<NaiveDate>,
                credit_days: Option<u32>,
            }
            let mut invoice_balances = HashMap::<ReferenceKey, PreviewBill>::new();
            let mut payment_references = HashMap::<ReferenceKey, HashSet<String>>::new();
            for invoice in self.invoices(company_id)? {
                let mut latest_reversal = crate::advances::invoice_allocation_state(
                    &self.conn, company_id, invoice.id, None,
                )?
                .latest_release
                .max(
                    crate::invoice_credits::state(&self.conn, company_id, invoice.id, None)?
                        .latest_reversal,
                );
                let invoice_key = (
                    normalized(invoice.kind.as_str()),
                    normalized(&invoice.party),
                    normalized(&invoice.reference),
                );
                let used = payment_references.entry(invoice_key.clone()).or_default();
                for settlement in self.invoice_settlements(company_id, invoice.id)? {
                    used.insert(normalized(&settlement.reference));
                    if let Some(date) = settlement.reversal_date {
                        let date = parse_date(&date)?;
                        latest_reversal =
                            Some(latest_reversal.map_or(date, |old: NaiveDate| old.max(date)));
                    }
                }
                let entries = if invoice.bill_splits.is_empty() {
                    vec![(
                        None,
                        invoice.bill_reference,
                        invoice.outstanding,
                        invoice.credit_days,
                    )]
                } else {
                    invoice
                        .bill_splits
                        .into_iter()
                        .map(|bill| {
                            let remaining = if invoice.reversal.is_some() {
                                0
                            } else {
                                bill.outstanding()
                            };
                            (Some(bill.id), bill.reference, remaining, bill.credit_days)
                        })
                        .collect()
                };
                for (split_id, reference, outstanding, credit_days) in entries {
                    let key = (
                        invoice_key.0.clone(),
                        invoice_key.1.clone(),
                        normalized(&reference),
                    );
                    if invoice_balances
                        .insert(
                            key,
                            PreviewBill {
                                target: Some(BillSettlementTarget {
                                    invoice_id: invoice.id,
                                    split_id,
                                }),
                                invoice_key: invoice_key.clone(),
                                invoice_date: invoice.date.clone(),
                                reference,
                                outstanding,
                                latest_reversal,
                                credit_days,
                            },
                        )
                        .is_some()
                    {
                        plan.issues
                            .push("Existing bill references are ambiguous".into());
                    }
                }
            }
            for invoice in &plan.invoices {
                let invoice_key = (
                    normalized(invoice.kind.as_str()),
                    normalized(&invoice.party),
                    normalized(&invoice.reference),
                );
                for bill in &invoice.bills {
                    if invoice.advance_bill_reference.as_ref() == Some(&bill.reference) {
                        continue;
                    }
                    let key = (
                        invoice_key.0.clone(),
                        invoice_key.1.clone(),
                        normalized(&bill.reference),
                    );
                    if invoice_balances
                        .insert(
                            key,
                            PreviewBill {
                                target: None,
                                invoice_key: invoice_key.clone(),
                                invoice_date: invoice.date.clone(),
                                reference: bill.reference.clone(),
                                outstanding: i128::from(bill.amount),
                                latest_reversal: None,
                                credit_days: bill.credit_days,
                            },
                        )
                        .is_some()
                    {
                        plan.issues
                            .push("Imported bill references are ambiguous".into());
                    }
                }
            }
            if let Some(opening_date) = &plan.opening_date {
                for account in &plan.accounts {
                    for bill in &account.opening_bills {
                        let key = (
                            normalized(bill.kind.as_str()),
                            normalized(&account.name),
                            normalized(&bill.reference),
                        );
                        if invoice_balances
                            .insert(
                                key.clone(),
                                PreviewBill {
                                    target: None,
                                    invoice_key: key,
                                    invoice_date: opening_date.clone(),
                                    reference: bill.reference.clone(),
                                    outstanding: i128::from(bill.amount),
                                    latest_reversal: None,
                                    credit_days: bill.credit_days,
                                },
                            )
                            .is_some()
                        {
                            plan.issues
                                .push("Opening bill references are ambiguous".into());
                        }
                    }
                }
            }
            for settlement in &mut plan.settlements {
                let allocations = &mut settlement.bills.invoices;
                let label = format!("Voucher {}", settlement.reference);
                let mut referenced_invoices = HashSet::new();
                for allocation in allocations {
                    let key = (
                        normalized(settlement.kind.as_str()),
                        normalized(&settlement.party),
                        normalized(&allocation.reference),
                    );
                    let Some(bill) = invoice_balances.get_mut(&key) else {
                        plan.issues.push(format!(
                            "{label}: referenced bill {} is unavailable",
                            allocation.reference
                        ));
                        continue;
                    };
                    if allocation.credit_days.is_some()
                        && allocation.credit_days != bill.credit_days
                    {
                        plan.issues.push(format!(
                            "{label}: bill credit period differs from the saved bill"
                        ));
                    }
                    if referenced_invoices.insert(bill.invoice_key.clone())
                        && !payment_references
                            .entry(bill.invoice_key.clone())
                            .or_default()
                            .insert(normalized(&settlement.reference))
                    {
                        plan.issues.push(format!(
                            "{label}: settlement reference already exists for the invoice"
                        ));
                    }
                    if bill.reference != allocation.reference {
                        plan.issues.push(format!(
                            "{label}: bill reference spelling differs from the saved bill"
                        ));
                    }
                    let date = parse_date(&settlement.date)?;
                    if date < parse_date(&bill.invoice_date)?
                        || bill.latest_reversal.is_some_and(|value| date < value)
                    {
                        plan.issues.push(format!("{label}: settlement precedes the invoice, a reversal or an advance release"));
                    }
                    if i128::from(allocation.amount) > bill.outstanding {
                        plan.issues.push(format!(
                            "{label}: allocation exceeds bill {}'s outstanding amount",
                            bill.reference
                        ));
                    }
                    allocation.existing_target = bill.target;
                    allocation.expected_outstanding = bill.outstanding;
                    bill.outstanding -= i128::from(allocation.amount);
                }
            }
        }
        for node in nodes().filter(|n| {
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
            && plan.settlements.is_empty()
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
        match plan.check_period_lock(&self.conn) {
            Ok(()) => {}
            Err(Error::Validation(message)) => plan.issues.push(message),
            Err(error) => return Err(error),
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
        plan.check_period_lock(&tx)?;
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
            let parent_id = match location.parent.as_deref() {
                Some(parent) => Some(
                    find_stock_location(&tx, plan.company_id, parent)?.ok_or_else(|| {
                        Error::Validation(format!(
                            "Stock location {}: parent {parent} is unavailable",
                            location.name
                        ))
                    })?,
                ),
                None => None,
            };
            tx.execute(
                "INSERT INTO stock_locations(company_id, name, parent_id) VALUES (?1, ?2, ?3)",
                params![plan.company_id, location.name, parent_id],
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
            if crate::opening::is_control_name(&account.name) {
                return Err(Error::Validation(
                    "A ledger name is reserved for opening balances".into(),
                ));
            }
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
            if let Some(days) = account.credit_days {
                tx.execute(
                    "INSERT INTO ledger_terms(company_id, ledger_id, credit_days) VALUES (?1, ?2, ?3)",
                    params![plan.company_id, ledger_id, days],
                )?;
            }
            if !account.opening_bills.is_empty() {
                let date = plan
                    .opening_date
                    .as_deref()
                    .ok_or_else(|| Error::Validation("Opening date is required".into()))?;
                for bill in &account.opening_bills {
                    crate::opening_bills::insert_opening_bill(
                        &tx,
                        plan.company_id,
                        &OpeningBillDraft {
                            kind: bill.kind,
                            party_ledger_id: ledger_id,
                            reference: bill.reference.clone(),
                            original_date: bill.original_date.clone(),
                            opening_date: date.to_owned(),
                            due_date: None,
                            credit_days: bill.credit_days,
                            amount: bill.amount,
                        },
                    )?;
                }
            } else if let Some(amount) = account.opening {
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
                    let adjustment = insert_stock_adjustment(
                        &tx,
                        plan.company_id,
                        item_id,
                        location,
                        batch,
                        date,
                        opening.quantity,
                        "Imported opening quantity",
                    )?;
                    if opening.quantity < 0 {
                        crate::stock_reservations::check_stock_coverage(
                            &tx,
                            plan.company_id,
                            item_id,
                            location,
                            batch,
                        )?;
                    }
                    if let Some(cost) = opening.cost {
                        crate::actual_cost::insert_receipt_cost(
                            &tx,
                            adjustment,
                            cost,
                            "Imported stock opening cost",
                        )?;
                    }
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
            let mut party_lines = Vec::new();
            for (index, line) in voucher.lines.iter().enumerate() {
                let ledger_id =
                    find_ledger(&tx, plan.company_id, &line.ledger)?.ok_or_else(|| {
                        Error::Validation(format!("Ledger {} is missing", line.ledger))
                    })?;
                postings.push(Posting {
                    ledger_id,
                    amount: line.amount,
                });
                if line.is_party {
                    party_lines.push(index);
                }
            }
            let id = insert_voucher_draft(
                &tx,
                plan.company_id,
                VoucherDraft {
                    kind: voucher.kind,
                    reference: &voucher.reference,
                    date: &voucher.date,
                    narration: &voucher.narration,
                    postings: &postings,
                    party_lines: &party_lines,
                },
            )?;
            save_metadata(&tx, plan.company_id, "voucher", id, &voucher.metadata)?;
        }
        let settlement_ledgers = |settlement: &ImportedSettlement| -> Result<(i64, i64)> {
            let kind = if settlement.kind == InvoiceKind::Sale {
                VoucherKind::Receipt
            } else {
                VoucherKind::Payment
            };
            let mut existing_vouchers =
                tx.prepare("SELECT kind, date, reference FROM vouchers WHERE company_id = ?1")?;
            for row in existing_vouchers.query_map([plan.company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (existing_kind, date, reference) = row?;
                if normalized(&existing_kind) == normalized(kind.as_str())
                    && xml_date(&date)? == settlement.date
                    && normalized(&reference) == normalized(&settlement.reference)
                {
                    return Err(Error::Validation(format!(
                        "Voucher {} already exists",
                        settlement.reference
                    )));
                }
            }
            drop(existing_vouchers);
            let bank_id =
                find_ledger(&tx, plan.company_id, &settlement.bank)?.ok_or_else(|| {
                    Error::Validation(format!("Ledger {} is missing", settlement.bank))
                })?;
            if settlement
                .existing_bank_id
                .is_some_and(|expected| expected != bank_id)
            {
                return Err(Error::Validation(format!(
                    "Ledger {} changed since preview",
                    settlement.bank
                )));
            }
            let party_id =
                find_ledger(&tx, plan.company_id, &settlement.party)?.ok_or_else(|| {
                    Error::Validation(format!("Ledger {} is missing", settlement.party))
                })?;
            if settlement
                .existing_party_id
                .is_some_and(|expected| expected != party_id)
            {
                return Err(Error::Validation(format!(
                    "Ledger {} changed since preview",
                    settlement.party
                )));
            }
            Ok((bank_id, party_id))
        };
        let mut advance_vouchers = HashMap::new();
        for (index, settlement) in plan.settlements.iter().enumerate() {
            let Some(ImportedAdvance {
                amount,
                source_bill,
            }) = &settlement.bills.advance
            else {
                continue;
            };
            let (bank_id, party_id) = settlement_ledgers(settlement)?;
            let (_, voucher_id) = insert_party_advance(
                &tx,
                plan.company_id,
                &AdvanceDraft {
                    kind: settlement.kind,
                    party_ledger_id: party_id,
                    bank_ledger_id: bank_id,
                    date: settlement.date.clone(),
                    reference: settlement.reference.clone(),
                    amount: *amount,
                    narration: settlement.narration.clone(),
                },
                Some(source_bill),
                settlement
                    .bills
                    .invoices
                    .iter()
                    .try_fold(0_i64, |total, allocation| {
                        total.checked_add(allocation.amount).ok_or_else(|| {
                            Error::Validation("Settlement total is too large".into())
                        })
                    })?,
            )?;
            advance_vouchers.insert(index, (bank_id, party_id, voucher_id));
            save_metadata(
                &tx,
                plan.company_id,
                "voucher",
                voucher_id,
                &settlement.metadata,
            )?;
        }
        let mut named_advances = if plan
            .invoices
            .iter()
            .any(|invoice| !invoice.advance_applications.is_empty())
        {
            read_named_advances(&tx, plan.company_id)?
        } else {
            HashMap::new()
        };
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
                let mut allocations = Vec::with_capacity(line.allocations.len());
                for allocation in &line.allocations {
                    let location_id =
                        find_stock_location(&tx, plan.company_id, &allocation.location)?
                            .ok_or_else(|| {
                                Error::Validation(format!(
                                    "Stock location {} is missing",
                                    allocation.location
                                ))
                            })?;
                    if allocation
                        .existing_location_id
                        .is_some_and(|previous| previous != location_id)
                    {
                        return Err(Error::Validation(format!(
                            "Stock location {} changed since preview",
                            allocation.location
                        )));
                    }
                    let batch_id = if let Some(name) = &allocation.batch {
                        let key = (item_id, normalized(name));
                        let existing = find_stock_batch(&tx, plan.company_id, item_id, name)?;
                        let id = if let Some(expected) = allocation.existing_batch_id {
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
                    allocations.push(InvoiceAllocation {
                        location_id,
                        batch_id,
                        quantity: allocation.quantity,
                    });
                }
                let mut allocations = allocations.into_iter();
                let primary = allocations
                    .next()
                    .ok_or_else(|| Error::Validation("Invoice allocation is missing".into()))?;
                lines.push(InvoiceLine {
                    item_id,
                    trade_ledger_id: {
                        let id = resolve_ledger(&line.trade_ledger, line.existing_trade_ledger_id)?;
                        (id != trade_id).then_some(id)
                    },
                    location_id: primary.location_id,
                    batch_id: primary.batch_id,
                    extra_allocations: allocations.collect(),
                    discount: line.discount.clone(),
                    quantity: line.quantity,
                    unit_price: line.unit_price,
                });
            }
            let draft = InvoiceDraft {
                kind: invoice.kind,
                entry_mode: invoice.entry_mode,
                date: invoice.date.clone(),
                due_date: if invoice.bills.len() == 1 {
                    invoice.bills[0].due_date.clone()
                } else {
                    None
                },
                credit_days: if invoice.bills.len() == 1 {
                    invoice.bills[0].credit_days
                } else {
                    None
                },
                reference: invoice.reference.clone(),
                bill_reference: (invoice.bills.len() == 1)
                    .then(|| invoice.bills[0].reference.clone()),
                bill_splits: if invoice.bills.len() > 1 {
                    invoice.bills.clone()
                } else {
                    Vec::new()
                },
                narration: invoice.narration.clone(),
                party_ledger_id: party_id,
                trade_ledger_id: trade_id,
                tax_ledger_id: None,
                tax_amount: 0,
                additional_taxes: Vec::new(),
                tax_calculation: None,
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
            let original_postings = invoice
                .original_postings
                .iter()
                .map(|line| {
                    Ok(Posting {
                        ledger_id: resolve_ledger(&line.ledger, None)?,
                        amount: line.amount,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let (invoice_id, voucher_id) = insert_direct_invoice(
                &tx,
                plan.company_id,
                &draft,
                StockInvoicePosting::Import {
                    original_postings: (invoice.entry_mode == InvoiceEntryMode::Voucher)
                        .then_some(original_postings.as_slice()),
                },
            )?;
            save_metadata(
                &tx,
                plan.company_id,
                "voucher",
                voucher_id,
                &invoice.metadata,
            )?;
            if let Some(reference) = &invoice.advance_bill_reference {
                let split_id = if draft.bill_splits.is_empty() {
                    None
                } else {
                    Some(
                        crate::bill_splits::read(&tx, invoice_id, None)?
                            .into_iter()
                            .find(|bill| &bill.reference == reference)
                            .ok_or_else(|| {
                                Error::Validation("Advance-funded bill is missing".into())
                            })?
                            .id,
                    )
                };
                let date = parse_date(&invoice.date)?;
                let target = BillSettlementTarget {
                    invoice_id,
                    split_id,
                };
                for application in &invoice.advance_applications {
                    let advance = named_advance(
                        &mut named_advances,
                        invoice.kind,
                        &invoice.party,
                        date,
                        application,
                    )?;
                    if (application.existing_advance_id.is_some()
                        && application.existing_advance_id != advance.id)
                        || advance.available != application.expected_available
                    {
                        return Err(Error::Validation(format!(
                            "Advance {} changed since preview",
                            application.reference
                        )));
                    }
                    let advance_id = advance
                        .id
                        .ok_or_else(|| Error::Validation("Named advance was not saved".into()))?;
                    insert_advance_allocation(
                        &tx,
                        plan.company_id,
                        advance_id,
                        target,
                        date,
                        application.amount,
                        &invoice.narration,
                    )?;
                    advance.available -= i128::from(application.amount);
                }
            }
        }
        for (index, settlement) in plan.settlements.iter().enumerate() {
            if settlement.bills.invoices.is_empty() {
                continue;
            }
            let (bank_id, party_id, advance_voucher) = match advance_vouchers.get(&index) {
                Some(&(bank, party, voucher)) => (bank, party, Some(voucher)),
                None => {
                    let (bank, party) = settlement_ledgers(settlement)?;
                    (bank, party, None)
                }
            };
            let mut allocations = Vec::new();
            for allocation in &settlement.bills.invoices {
                let mut stmt = tx.prepare(
                    "SELECT i.id, b.id, COALESCE(b.reference, i.bill_reference, v.reference), i.tax_amount,
                            CASE WHEN b.id IS NULL THEN i.credit_days ELSE b.credit_days END
                     FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
                     LEFT JOIN invoice_bill_splits b ON b.invoice_id = i.id
                     WHERE i.company_id = ?1 AND i.kind = ?2 AND i.party_ledger_id = ?3",
                )?;
                let mut found = None;
                for row in stmt.query_map(
                    params![plan.company_id, settlement.kind.as_str(), party_id],
                    |r| {
                        Ok((
                            BillSettlementTarget {
                                invoice_id: r.get(0)?,
                                split_id: r.get(1)?,
                            },
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, Option<u32>>(4)?,
                        ))
                    },
                )? {
                    let (target, reference, tax, credit_days) = row?;
                    if normalized(&reference) == normalized(&allocation.reference) {
                        if found.is_some() || reference != allocation.reference {
                            return Err(Error::Validation(
                                "Bill reference changed or became ambiguous since preview".into(),
                            ));
                        }
                        found = Some((target, tax, credit_days));
                    }
                }
                drop(stmt);
                let (target, tax, credit_days) = found.ok_or_else(|| {
                    Error::Validation(format!("Bill {} is missing", allocation.reference))
                })?;
                let changed = || {
                    Error::Validation(format!(
                        "Bill {} changed since preview",
                        allocation.reference
                    ))
                };
                if allocation.credit_days.is_some() && allocation.credit_days != credit_days
                    || allocation
                        .existing_target
                        .is_some_and(|expected| expected != target)
                {
                    return Err(changed());
                }
                let id = target.invoice_id;
                let outstanding = if let Some(split_id) = target.split_id {
                    let bill = crate::bill_splits::read(&tx, id, None)?
                        .into_iter()
                        .find(|bill| bill.id == split_id)
                        .ok_or_else(changed)?;
                    bill.outstanding()
                } else {
                    let advances =
                        crate::advances::invoice_allocation_state(&tx, plan.company_id, id, None)?;
                    let credits = crate::invoice_credits::state(&tx, plan.company_id, id, None)?;
                    let mut outstanding =
                        invoice_total(&tx, id, tax)? - advances.applied - credits.applied;
                    let mut paid = tx.prepare(
                        "SELECT s.amount FROM invoice_settlements s WHERE s.company_id = ?1 AND s.invoice_id = ?2
                         AND NOT EXISTS(SELECT 1 FROM settlement_reversals r
                             WHERE r.company_id = s.company_id AND r.settlement_voucher_id = s.voucher_id)",
                    )?;
                    for amount in
                        paid.query_map(params![plan.company_id, id], |r| r.get::<_, i64>(0))?
                    {
                        outstanding -= i128::from(amount?);
                    }
                    outstanding
                };
                if outstanding != allocation.expected_outstanding {
                    return Err(changed());
                }
                allocations.push(BillSettlementAllocation {
                    target,
                    amount: allocation.amount,
                });
            }
            let (voucher_id, _) = insert_settlement_on_connection(
                &tx,
                plan.company_id,
                bank_id,
                &settlement.date,
                &settlement.reference,
                &allocations,
                &settlement.narration,
                advance_voucher,
            )?;
            if advance_voucher.is_none() {
                save_metadata(
                    &tx,
                    plan.company_id,
                    "voucher",
                    voucher_id,
                    &settlement.metadata,
                )?;
            }
        }
        crate::stock_policy::check_import(&tx, plan.company_id)?;
        tx.commit()?;
        Ok(ImportCounts {
            groups: plan.groups.len(),
            ledgers: plan.accounts.len(),
            opening_bills: plan.opening_bill_count(),
            vouchers: plan.transactions.len(),
            invoices: plan.invoices.len(),
            settlements: plan.settlement_count(),
            advances: plan.advance_count(),
            advance_applications: plan.advance_application_count(),
            units: plan.units.len(),
            stock_groups: plan.stock_groups.len(),
            locations: plan.locations.len(),
            items: plan.items.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_advances_reconcile_on_fully_and_partially_funded_imported_invoices() {
        for (kind, cash_kind, party_category, trade_category, invoice_amount, cash_amount) in [
            ("Sales", "Receipt", Category::Asset, Category::Income, -1, 1),
            (
                "Purchase",
                "Payment",
                Category::Liability,
                Category::Expense,
                1_i64,
                -1_i64,
            ),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Migration").unwrap();
            let party = store
                .create_ledger(company, "Party", party_category)
                .unwrap();
            let bank = store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            store
                .create_ledger(company, "Trade", trade_category)
                .unwrap();
            let cash_voucher = |date, number, bill, bill_type, amount| {
                let party_amount: i64 = cash_amount * amount;
                format!(
                    r#"<VOUCHER>
                    <DATE>{date}</DATE><VOUCHERTYPENAME>{cash_kind}</VOUCHERTYPENAME><VOUCHERNUMBER>{number}</VOUCHERNUMBER>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>{party_amount}</AMOUNT>
                        <BILLALLOCATIONS.LIST><NAME>{bill}</NAME><BILLTYPE>{bill_type}</BILLTYPE>
                            <AMOUNT>{party_amount}</AMOUNT></BILLALLOCATIONS.LIST>
                    </LEDGERENTRIES.LIST>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>{}</AMOUNT></LEDGERENTRIES.LIST>
                </VOUCHER>"#,
                    -party_amount
                )
            };
            let allocation = |reference, bill_type, amount| {
                format!(
                "<BILLALLOCATIONS.LIST><NAME>{reference}</NAME><BILLTYPE>{bill_type}</BILLTYPE><AMOUNT>{}</AMOUNT></BILLALLOCATIONS.LIST>",
                invoice_amount * amount,
            )
            };
            let invoice = |date, number, amount, bills: String| {
                let party_amount: i64 = invoice_amount * amount;
                format!(
                    r#"<VOUCHER>
                    <DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME><VOUCHERNUMBER>{number}</VOUCHERNUMBER>
                    <ISINVOICE>Yes</ISINVOICE>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><ISPARTYLEDGER>Yes</ISPARTYLEDGER>
                        <AMOUNT>{party_amount}</AMOUNT>{bills}
                    </LEDGERENTRIES.LIST>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>{}</AMOUNT></LEDGERENTRIES.LIST>
                </VOUCHER>"#,
                    -party_amount
                )
            };
            // Keep the source advance bill name distinct from the cash voucher number.
            let advance = cash_voucher("20260101", "CASH-ADV", "DEPOSIT", "Advance", 80);
            let first = invoice(
                "20260110",
                "INV-1",
                100,
                allocation("DEPOSIT", "Agst Ref", 50) + &allocation("OPEN", "New Ref", 50),
            );
            let second = invoice(
                "20260112",
                "INV-2",
                25,
                allocation("DEPOSIT", "Agst Ref", 25),
            );
            let payment = cash_voucher("20260115", "CASH-DUE", "OPEN", "Agst Ref", 15);
            let envelope = |data: &str| {
                format!("<ENVELOPE><BODY><DATA><MESSAGE>{data}</MESSAGE></DATA></BODY></ENVELOPE>")
            };
            let transactions = envelope(&format!("{first}{payment}{second}{advance}"));
            for (bad_export, issue) in [
                (envelope(&first), "unavailable"),
                (
                    transactions.replace("<DATE>20260101</DATE>", "<DATE>20260111</DATE>"),
                    "precedes advance",
                ),
                (
                    transactions
                        .replace(">80<", ">65<")
                        .replace(">-80<", ">-65<"),
                    "exceeds the available",
                ),
                (
                    transactions.replace(
                        "<BILLTYPE>Advance</BILLTYPE>",
                        "<BILLTYPE>On Account</BILLTYPE>",
                    ),
                    "unavailable",
                ),
            ] {
                let blocked = store
                    .preview_xml_import(company, bad_export.as_bytes())
                    .unwrap();
                assert!(
                    blocked.issues().iter().any(|row| row.contains(issue)),
                    "{:?}",
                    blocked.issues()
                );
                assert!(store.apply_xml_import(&blocked).is_err());
            }
            assert!(store.vouchers(company).unwrap().is_empty());

            let plan = store
                .preview_xml_import(company, transactions.as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (
                    counts.invoices,
                    counts.advances,
                    counts.advance_applications,
                    counts.settlements
                ),
                (2, 1, 2, 1)
            );
            let advances = store.party_advances(company, None).unwrap();
            let advance = &advances[0];
            assert_eq!((advance.applied, advance.available), (7500, 500));
            assert_eq!(
                advance.source_bill.as_ref().unwrap().reference.as_deref(),
                Some("DEPOSIT")
            );
            let history = store
                .advance_allocations(company, advance.id, None)
                .unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|row| (row.invoice_reference.as_str(), row.amount))
                    .collect::<Vec<_>>(),
                [("INV-1", 5000), ("INV-2", 2500)]
            );
            assert!(store
                .outstanding_invoices(company, "2026-01-09")
                .unwrap()
                .is_empty());

            for (date, unpaid, unspent, bank_total) in [
                ("2026-01-10", 5000, 3000, 8000),
                ("2026-01-12", 5000, 500, 8000),
                ("2026-01-15", 3500, 500, 9500),
            ] {
                let bills = store.outstanding_invoices(company, date).unwrap();
                assert_eq!(bills.len(), 1);
                assert_eq!(
                    (&*bills[0].bill_reference, bills[0].outstanding),
                    ("OPEN", unpaid)
                );
                let balance = |ledger| {
                    store
                        .ledger_register(company, ledger, None, Some(date))
                        .unwrap()
                        .closing_balance
                };
                assert_eq!(balance(party), i128::from(cash_amount) * (unpaid - unspent));
                assert_eq!(balance(bank), i128::from(cash_amount) * bank_total);
            }
            let final_invoice =
                invoice("20260117", "INV-3", 5, allocation("DEPOSIT", "Agst Ref", 5));
            let export = envelope(&final_invoice);
            let stale = store
                .preview_xml_import(company, export.as_bytes())
                .unwrap();
            assert!(stale.can_apply(), "{:?}", stale.issues());
            let open = &store.outstanding_invoices(company, "2026-01-16").unwrap()[0];
            store
                .apply_party_advance_to_bill(
                    company,
                    advance.id,
                    BillSettlementTarget {
                        invoice_id: open.invoice_id,
                        split_id: open.split_id,
                    },
                    "2026-01-16",
                    100,
                    "Apply part before import",
                )
                .unwrap();
            assert!(store.apply_xml_import(&stale).is_err());
            assert_eq!(store.invoices(company).unwrap().len(), 2);
            assert_eq!(store.vouchers(company).unwrap().len(), 4);

            let current = invoice("20260117", "INV-3", 4, allocation("DEPOSIT", "Agst Ref", 4));
            let plan = store
                .preview_xml_import(company, envelope(&current).as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            store.apply_xml_import(&plan).unwrap();
            assert_eq!(store.party_advances(company, None).unwrap()[0].available, 0);
        }
    }

    #[test]
    fn mixed_receipt_keeps_one_cash_posting_and_reverses_both_parts() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Migration").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let bank = store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let invoice = |date, number, amount, bills: &str| {
            format!(
                r#"<VOUCHER>
            <DATE>{date}</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME><VOUCHERNUMBER>{number}</VOUCHERNUMBER>
            <ISINVOICE>Yes</ISINVOICE>
            <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><ISPARTYLEDGER>Yes</ISPARTYLEDGER>
                <AMOUNT>-{amount}</AMOUNT>{bills}
            </LEDGERENTRIES.LIST>
            <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>{amount}</AMOUNT></LEDGERENTRIES.LIST>
        </VOUCHER>"#
            )
        };
        let bill = |name, bill_type, amount| {
            format!(
            "<BILLALLOCATIONS.LIST><NAME>{name}</NAME><BILLTYPE>{bill_type}</BILLTYPE><AMOUNT>{amount}</AMOUNT></BILLALLOCATIONS.LIST>"
        )
        };
        let first = invoice(
            "20260101",
            "INV-1",
            100,
            &(bill("A", "New Ref", -40) + &bill("B", "New Ref", -60)),
        );
        let later = invoice(
            "20260115",
            "INV-2",
            20,
            &bill("RC-DEPOSIT", "Agst Ref", -20),
        );
        let allocations = bill("A", "Agst Ref", 25) + &bill("B", "Agst Ref", 35);
        let deposit = bill("RC-DEPOSIT", "Advance", 30);
        let xml = |deposit: &str| {
            format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
            {later}<VOUCHER>
                <DATE>20260110</DATE><VOUCHERTYPENAME>Receipt</VOUCHERTYPENAME>
                <VOUCHERNUMBER>RC-01</VOUCHERNUMBER><GUID>mixed-receipt-example</GUID>
                <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>90</AMOUNT>{allocations}{deposit}</LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>-90</AMOUNT></LEDGERENTRIES.LIST>
            </VOUCHER>{first}</MESSAGE></DATA></BODY></ENVELOPE>"#
            )
        };
        for (invalid, expected) in [
            (
                xml(&bill("RC-DEPOSIT", "Advance", 29)),
                "do not equal the party",
            ),
            (
                xml(&(bill("RC-DEPOSIT", "Advance", 20) + &bill("", "On Account", 10))),
                "only one advance",
            ),
        ] {
            let plan = store
                .preview_xml_import(company, invalid.as_bytes())
                .unwrap();
            assert!(
                plan.issues().iter().any(|issue| issue.contains(expected)),
                "{:?}",
                plan.issues()
            );
            assert!(store.apply_xml_import(&plan).is_err());
        }
        assert!(store.vouchers(company).unwrap().is_empty());
        let plan = store
            .preview_xml_import(company, xml(&deposit).as_bytes())
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        let counts = store.apply_xml_import(&plan).unwrap();
        assert_eq!(
            (
                counts.invoices,
                counts.settlements,
                counts.advances,
                counts.advance_applications
            ),
            (2, 1, 1, 1)
        );
        assert_eq!(store.vouchers(company).unwrap().len(), 3);
        let ledger = |s: &Store, ledger| {
            s.ledger_register(company, ledger, None, None)
                .unwrap()
                .closing_balance
        };
        assert_eq!(ledger(&store, bank), 9000);
        assert_eq!(ledger(&store, customer), 3000);
        let rows = store.outstanding_invoices(company, "2026-01-15").unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| (row.bill_reference.as_str(), row.outstanding))
                .collect::<Vec<_>>(),
            [("A", 1500), ("B", 2500)]
        );
        let settlement = store
            .invoice_settlements(company, rows[0].invoice_id)
            .unwrap()
            .remove(0);
        let advance = store.party_advances(company, None).unwrap().remove(0);
        assert_eq!(settlement.voucher_id, advance.voucher_id);
        use crate::cash_flow::{CashClass, CashRole};
        store
            .save_cash_ledger_rule(company, bank, CashRole::Cash, "Test bank classification")
            .unwrap();
        store
            .save_cash_voucher_rule(
                company,
                settlement.voucher_id,
                Some(CashClass::Operating),
                "Test receipt classification",
            )
            .unwrap();
        assert_eq!(
            (settlement.amount, advance.amount, advance.available),
            (6000, 3000, 1000)
        );
        assert!(advance.includes_settlements);
        let partial_reversal = store.conn.execute(
            "UPDATE party_advances SET reversal_voucher_id = ?1, reversal_reason = 'Test partial reversal' WHERE id = ?2",
            params![settlement.voucher_id, advance.id],
        ).unwrap_err();
        assert!(partial_reversal
            .to_string()
            .contains("Reverse the entire combined receipt or payment"));
        assert!(store
            .reverse_settlement(
                company,
                settlement.voucher_id,
                "2026-01-20",
                "REV",
                "Test reversal"
            )
            .is_err());
        assert!(store
            .reverse_party_advance(company, advance.id, "2026-01-20", "REV", "Test reversal")
            .is_err());
        assert_eq!(ledger(&store, bank), 9000);
        let history = store
            .advance_allocations(company, advance.id, None)
            .unwrap();
        store
            .release_advance_allocation(company, history[0].id, "2026-01-18", "Test release")
            .unwrap();
        assert!(store
            .reverse_settlement(
                company,
                settlement.voucher_id,
                "2026-01-17",
                "REV",
                "Test reversal"
            )
            .is_err());
        let reversed = store
            .reverse_party_advance(company, advance.id, "2026-01-20", "REV", "Test reversal")
            .unwrap();
        assert_eq!(store.vouchers(company).unwrap().len(), 4);
        assert_eq!(ledger(&store, bank), 0);
        assert_eq!(ledger(&store, customer), 12000);
        let cash = store.cash_flow_report(company, None, None).unwrap();
        assert_eq!(cash.entries.len(), 2);
        let operating = cash.totals[&CashClass::Operating];
        assert_eq!(
            (cash.closing, operating.inflows, operating.outflows),
            (0, 9000, 9000)
        );
        assert_eq!(
            store
                .invoice_settlements(company, rows[0].invoice_id)
                .unwrap()[0]
                .reversal_voucher_id,
            Some(reversed)
        );
        let reversed_advance = store.party_advances(company, None).unwrap().remove(0);
        assert_eq!(
            (
                reversed_advance.available,
                reversed_advance.reversal.unwrap().voucher_id
            ),
            (0, reversed)
        );
        for (date, amounts) in [
            ("2026-01-19", vec![1500, 2500, 2000]),
            ("2026-01-20", vec![4000, 6000, 2000]),
        ] {
            assert_eq!(
                store
                    .outstanding_invoices(company, date)
                    .unwrap()
                    .iter()
                    .map(|row| row.outstanding)
                    .collect::<Vec<_>>(),
                amounts
            );
        }

        let target = BillSettlementTarget {
            invoice_id: rows[0].invoice_id,
            split_id: rows[0].split_id,
        };
        assert!(store
            .settle_bills_with_advance(
                company,
                bank,
                "2026-01-21",
                "MANUAL",
                &[BillSettlementAllocation {
                    target,
                    amount: 4001
                }],
                500
            )
            .is_err());
        assert_eq!(store.vouchers(company).unwrap().len(), 4);
        let (native, _) = store
            .settle_bills_with_advance(
                company,
                bank,
                "2026-01-21",
                "MANUAL",
                &[BillSettlementAllocation {
                    target,
                    amount: 1500,
                }],
                500,
            )
            .unwrap();
        assert_eq!(ledger(&store, bank), 2000);
        assert_eq!(
            store.party_advances(company, None).unwrap()[0].available,
            500
        );
        store
            .reverse_settlement(company, native, "2026-01-22", "MANUAL-REV", "Test reversal")
            .unwrap();
        assert_eq!(ledger(&store, bank), 0);
        assert_eq!(store.vouchers(company).unwrap().len(), 6);
    }

    #[test]
    fn split_bills_keep_their_terms_and_cash_allocations_through_reversal() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let bank = store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        let invoice = br#"<ENVELOPE><BODY><DATA><MESSAGE><VOUCHER>
            <DATE>20260101</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
            <VOUCHERNUMBER>INV-01</VOUCHERNUMBER><ISINVOICE>Yes</ISINVOICE>
            <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME>
                <ISPARTYLEDGER>Yes</ISPARTYLEDGER><AMOUNT>-100.00</AMOUNT>
                <BILLALLOCATIONS.LIST><NAME>FIRST</NAME><BILLTYPE>New Ref</BILLTYPE>
                    <AMOUNT>-35.00</AMOUNT><BILLCREDITPERIOD>10 Days</BILLCREDITPERIOD></BILLALLOCATIONS.LIST>
                <BILLALLOCATIONS.LIST><NAME>SECOND</NAME><BILLTYPE>New Ref</BILLTYPE>
                    <AMOUNT>-65.00</AMOUNT><BILLCREDITPERIOD>31-Jan-2026</BILLCREDITPERIOD></BILLALLOCATIONS.LIST>
            </LEDGERENTRIES.LIST>
            <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>100.00</AMOUNT></LEDGERENTRIES.LIST>
        </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#;
        let receipt = br#"<ENVELOPE><BODY><DATA><MESSAGE><VOUCHER>
            <DATE>20260108</DATE><VOUCHERTYPENAME>Receipt</VOUCHERTYPENAME><VOUCHERNUMBER>RC-01</VOUCHERNUMBER>
            <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>40.00</AMOUNT>
                <BILLALLOCATIONS.LIST><NAME>FIRST</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>15.00</AMOUNT></BILLALLOCATIONS.LIST>
                <BILLALLOCATIONS.LIST><NAME>SECOND</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>25.00</AMOUNT></BILLALLOCATIONS.LIST>
            </LEDGERENTRIES.LIST>
            <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>-40.00</AMOUNT></LEDGERENTRIES.LIST>
        </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#;
        let plan = store
            .preview_xml_import_batch(company, &[invoice, receipt], None)
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        let imported = store.apply_xml_import(&plan).unwrap();
        assert_eq!((imported.invoices, imported.settlements), (1, 1));
        let row = store
            .invoice_register(company, None, None, None, None, "SECOND")
            .unwrap()
            .remove(0);
        assert_eq!(row.total, 10000);
        assert_eq!(row.outstanding, 6000);
        let settlement = store
            .invoice_settlements(company, row.id)
            .unwrap()
            .remove(0);
        assert_eq!(settlement.amount, 4000);
        assert_eq!(
            settlement.bill_allocations,
            [("FIRST".into(), 1500), ("SECOND".into(), 2500)]
        );
        let report = store.outstanding_invoices(company, "2026-01-15").unwrap();
        assert_eq!(report[1].due_date.as_deref(), Some("2026-01-31"));
        assert_eq!(
            report
                .iter()
                .map(|bill| (
                    bill.bill_reference.as_str(),
                    bill.outstanding,
                    bill.overdue_days
                ))
                .collect::<Vec<_>>(),
            [("FIRST", 2000, Some(4)), ("SECOND", 4000, Some(0))]
        );
        let totals = store.party_outstanding(company, "2026-01-15").unwrap();
        assert_eq!(
            (
                totals[0].invoices,
                totals[0].receivable,
                totals[0].overdue_receivable
            ),
            (1, 6000, 2000)
        );

        let first = BillSettlementTarget {
            invoice_id: row.id,
            split_id: report[0].split_id,
        };
        assert!(store
            .settle_bills(
                company,
                bank,
                "2026-01-15",
                "TOO-MUCH",
                &[BillSettlementAllocation {
                    target: first,
                    amount: 2001
                }]
            )
            .is_err());
        assert!(store
            .settle_invoice(company, row.id, bank, "2026-01-15", "NO-BILL", 1)
            .is_err());
        store
            .reverse_settlement(
                company,
                settlement.voucher_id,
                "2026-01-20",
                "REVERSE",
                "Wrong receipt",
            )
            .unwrap();
        let prior = store.outstanding_invoices(company, "2026-01-19").unwrap();
        let reversed = store.outstanding_invoices(company, "2026-01-20").unwrap();
        assert_eq!(
            prior
                .iter()
                .map(|bill| bill.outstanding)
                .collect::<Vec<_>>(),
            [2000, 4000]
        );
        assert_eq!(
            reversed
                .iter()
                .map(|bill| bill.outstanding)
                .collect::<Vec<_>>(),
            [3500, 6500]
        );
    }

    #[test]
    fn accounting_trade_vouchers_preserve_stock_postings_and_bill_links() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let sales = store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        store
            .set_negative_stock_policy(company, Some("2026-01-01"), "Check stock")
            .unwrap();
        let allocation = |quantity: &str, amount: &str, rate: &str| {
            format!(
                r#"<INVENTORYALLOCATIONS.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>{rate}
                <AMOUNT>{amount}</AMOUNT><ACTUALQTY>{quantity} Each</ACTUALQTY>
                <BILLEDQTY>{quantity} Each</BILLEDQTY>
                <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><BATCHNAME>Lot-1</BATCHNAME>
                    <AMOUNT>{amount}</AMOUNT><ACTUALQTY>{quantity} Each</ACTUALQTY>
                    <BILLEDQTY>{quantity} Each</BILLEDQTY>
                </BATCHALLOCATIONS.LIST></INVENTORYALLOCATIONS.LIST>"#
            )
        };
        let purchased = allocation("4", "-100.00", "");
        let first_sale = allocation("1", "40.00", "");
        let second_sale = allocation("2", "70.00", "<RATE>35.00/Each</RATE>");
        let xml = format!(
            r#"<ENVELOPE><BODY><DATA><MESSAGE>
            <VOUCHER OBJVIEW="Accounting Voucher View">
                <DATE>20260102</DATE><VOUCHERTYPENAME>Purchase</VOUCHERTYPENAME>
                <VOUCHERNUMBER>PV-01</VOUCHERNUMBER><PARTYLEDGERNAME>Supplier</PARTYLEDGERNAME>
                <ALLLEDGERENTRIES.LIST><LEDGERNAME>Purchases</LEDGERNAME><AMOUNT>-100.00</AMOUNT>
                    {purchased}
                </ALLLEDGERENTRIES.LIST>
                <ALLLEDGERENTRIES.LIST><LEDGERNAME>Supplier</LEDGERNAME><AMOUNT>100.00</AMOUNT>
                    <BILLALLOCATIONS.LIST><NAME>SUP-01</NAME><BILLTYPE>New Ref</BILLTYPE>
                        <AMOUNT>100.00</AMOUNT></BILLALLOCATIONS.LIST>
                </ALLLEDGERENTRIES.LIST>
            </VOUCHER>
            <VOUCHER VCHTYPE="Sales" OBJVIEW="Accounting Voucher View">
                <DATE>20260103</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
                <VOUCHERNUMBER>SV-01</VOUCHERNUMBER><ISINVOICE>No</ISINVOICE>
                <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME>
                    <ISPARTYLEDGER>Yes</ISPARTYLEDGER><AMOUNT>-110.00</AMOUNT>
                    <BILLALLOCATIONS.LIST><NAME>CUST-01</NAME><BILLTYPE>New Ref</BILLTYPE>
                        <AMOUNT>-110.00</AMOUNT></BILLALLOCATIONS.LIST>
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>40.00</AMOUNT>
                    {first_sale}
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>70.00</AMOUNT>
                    {second_sale}
                </LEDGERENTRIES.LIST>
            </VOUCHER>
            <VOUCHER>
                <DATE>20260104</DATE><VOUCHERTYPENAME>Receipt</VOUCHERTYPENAME>
                <VOUCHERNUMBER>RC-01</VOUCHERNUMBER>
                <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>25.00</AMOUNT>
                    <BILLALLOCATIONS.LIST><NAME>CUST-01</NAME><BILLTYPE>Agst Ref</BILLTYPE>
                        <AMOUNT>25.00</AMOUNT></BILLALLOCATIONS.LIST>
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>-25.00</AMOUNT>
                </LEDGERENTRIES.LIST>
            </VOUCHER>
        </MESSAGE></DATA></BODY></ENVELOPE>"#
        );
        let mismatch = xml.replace(
            "<LEDGERNAME>Sales</LEDGERNAME><AMOUNT>70.00",
            "<LEDGERNAME>Sales</LEDGERNAME><AMOUNT>71.00",
        );
        let blocked = store
            .preview_xml_import(company, mismatch.as_bytes())
            .unwrap();
        assert!(!blocked.can_apply());
        assert!(blocked
            .issues()
            .iter()
            .any(|issue| issue.contains("Stock amounts do not reconcile")));
        assert!(store.apply_xml_import(&blocked).is_err());
        assert!(store.vouchers(company).unwrap().is_empty());

        let plan = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        let counts = store.apply_xml_import(&plan).unwrap();
        assert_eq!((counts.invoices, counts.settlements), (2, 1));
        assert_eq!(
            store
                .stock_location_balances(company, location, Some("2026-01-04"))
                .unwrap()[0]
                .quantity,
            1000
        );
        let bills = store.outstanding_invoices(company, "2026-01-04").unwrap();
        let receivable = bills
            .iter()
            .find(|bill| bill.party_ledger_id == customer)
            .unwrap();
        let payable = bills
            .iter()
            .find(|bill| bill.party_ledger_id == supplier)
            .unwrap();
        assert_eq!(
            (receivable.bill_reference.as_str(), receivable.outstanding),
            ("CUST-01", 8500)
        );
        assert_eq!(
            (payable.bill_reference.as_str(), payable.outstanding),
            ("SUP-01", 10000)
        );
        assert!(bills
            .iter()
            .all(|bill| bill.entry_mode == InvoiceEntryMode::Voucher));
        let register = store
            .invoice_register(company, None, None, None, None, "SV-01")
            .unwrap();
        assert_eq!(register[0].document_type(), "Sales voucher");
        assert_eq!(register[0].lines[0].unit_price, 4000);
        assert_eq!(register[0].lines[1].unit_price, 3500);
        let postings = store.vouchers(company).unwrap();
        let sale = postings
            .iter()
            .find(|voucher| voucher.id == register[0].voucher_id)
            .unwrap();
        assert_eq!(sale.party_lines, [0]);
        assert_eq!(
            sale.postings
                .iter()
                .map(|row| (row.ledger_id, row.amount))
                .collect::<Vec<_>>(),
            [(customer, 11000), (sales, -4000), (sales, -7000)]
        );
    }

    #[test]
    fn receipts_resolve_bill_references_independently_of_document_numbers() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        let invoice = |reference: &str, bill: &str| {
            format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
                <VOUCHER>
                    <DATE>20260102</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
                    <VOUCHERNUMBER>{reference}</VOUCHERNUMBER><ISINVOICE>Yes</ISINVOICE>
                    <PERSISTEDVIEW>Invoice Voucher View</PERSISTEDVIEW>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME>
                        <ISPARTYLEDGER>Yes</ISPARTYLEDGER><AMOUNT>-100.00</AMOUNT>
                        <BILLALLOCATIONS.LIST><NAME>{bill}</NAME><BILLTYPE>New Ref</BILLTYPE>
                            <BILLCREDITPERIOD>30 Days</BILLCREDITPERIOD><AMOUNT>-100.00</AMOUNT>
                        </BILLALLOCATIONS.LIST>
                    </LEDGERENTRIES.LIST>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>100.00</AMOUNT>
                    </LEDGERENTRIES.LIST>
                </VOUCHER>
            </MESSAGE></DATA></BODY></ENVELOPE>"#
            )
        };
        let receipt = |reference: &str, bill: &str, amount: &str| {
            format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
                <VOUCHER>
                    <DATE>20260105</DATE><VOUCHERTYPENAME>Receipt</VOUCHERTYPENAME>
                    <VOUCHERNUMBER>{reference}</VOUCHERNUMBER>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME>
                        <ISPARTYLEDGER>Yes</ISPARTYLEDGER><AMOUNT>{amount}</AMOUNT>
                        <BILLALLOCATIONS.LIST><NAME>{bill}</NAME><BILLTYPE>Agst Ref</BILLTYPE>
                            <AMOUNT>{amount}</AMOUNT></BILLALLOCATIONS.LIST>
                    </LEDGERENTRIES.LIST>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>-{amount}</AMOUNT>
                    </LEDGERENTRIES.LIST>
                </VOUCHER>
            </MESSAGE></DATA></BODY></ENVELOPE>"#
            )
        };
        let sale = invoice("INV-01", "BILL-07").replace("30 Days", "20260201");
        let other = invoice("BILL-07", "INV-01")
            .replace("<ISINVOICE>Yes", "<ISINVOICE>No")
            .replace("Invoice Voucher View", "Accounting Voucher View");
        let paid = receipt("RCPT-01", "BILL-07", "40.00");
        let plan = store
            .preview_xml_import_batch(
                company,
                &[sale.as_bytes(), other.as_bytes(), paid.as_bytes()],
                None,
            )
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        let counts = store.apply_xml_import(&plan).unwrap();
        assert_eq!(counts.invoices, 2);
        assert_eq!(counts.settlements, 1);
        let outstanding = store.outstanding_invoices(company, "2026-01-05").unwrap();
        let first = outstanding
            .iter()
            .find(|row| row.reference == "INV-01")
            .unwrap();
        assert_eq!(first.bill_reference, "BILL-07");
        assert_eq!(first.due_date.as_deref(), Some("2026-02-01"));
        assert_eq!(first.outstanding, 6000);
        let second = outstanding
            .iter()
            .find(|row| row.reference == "BILL-07")
            .unwrap();
        assert_eq!(second.outstanding, 10000);

        let stale = receipt("RCPT-02", "INV-01", "25.00");
        let stale = store.preview_xml_import(company, stale.as_bytes()).unwrap();
        assert!(stale.can_apply(), "{:?}", stale.issues());
        let mut draft = store.invoice_draft(company, second.invoice_id).unwrap();
        assert_eq!(draft.entry_mode, InvoiceEntryMode::Voucher);
        assert_eq!(draft.bill_reference.as_deref(), Some("INV-01"));
        draft.bill_reference = Some("BILL-08".into());
        store
            .update_invoice(company, second.invoice_id, &draft)
            .unwrap();
        let revision = store
            .invoice_revisions(company, second.invoice_id)
            .unwrap()
            .remove(0);
        assert_eq!(revision.bill_reference, "INV-01");
        assert_eq!(revision.entry_mode, InvoiceEntryMode::Voucher);
        assert!(store.apply_xml_import(&stale).is_err());

        let duplicate = invoice("INV-03", "bill-08");
        let duplicate = store
            .preview_xml_import(company, duplicate.as_bytes())
            .unwrap();
        assert!(!duplicate.can_apply());
        assert!(duplicate
            .issues()
            .iter()
            .any(|issue| issue.contains("bill reference bill-08 already exists")));
        let paid = receipt("RCPT-02", "BILL-08", "25.00");
        let plan = store.preview_xml_import(company, paid.as_bytes()).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let rows = store
            .invoice_register(company, None, None, None, None, "BILL-08")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reference, "BILL-07");
        assert_eq!(rows[0].outstanding, 7500);
        assert_eq!(
            store
                .ledger_register(company, customer, None, None)
                .unwrap()
                .closing_balance,
            13500
        );
    }

    #[test]
    fn ordinary_voucher_party_lines_survive_import_edits_and_reversal() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let xml = r#"<ENVELOPE><BODY><DATA><MESSAGE>
            <VOUCHER>
                <DATE>20260102</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
                <VOUCHERNUMBER>S-1</VOUCHERNUMBER>
                <PERSISTEDVIEW>Accounting Voucher View</PERSISTEDVIEW><ISINVOICE>No</ISINVOICE>
                <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME>
                    <ISDEEMEDPOSITIVE>Yes</ISDEEMEDPOSITIVE>
                    <ISPARTYLEDGER>Yes</ISPARTYLEDGER><AMOUNT>-40.00</AMOUNT>
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME>
                    <ISPARTYLEDGER>No</ISPARTYLEDGER><AMOUNT>-10.00</AMOUNT>
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME>
                    <ISDEEMEDPOSITIVE>No</ISDEEMEDPOSITIVE><AMOUNT>50.00</AMOUNT>
                </LEDGERENTRIES.LIST>
            </VOUCHER>
        </MESSAGE></DATA></BODY></ENVELOPE>"#;
        let invalid = xml.replace("<ISPARTYLEDGER>Yes", "<ISPARTYLEDGER>Unknown");
        let preview = store
            .preview_xml_import(company, invalid.as_bytes())
            .unwrap();
        assert!(!preview.can_apply());
        assert!(preview
            .issues()
            .iter()
            .any(|issue| issue.contains("invalid party flag")));
        let with_bill = xml.replace(
            "<AMOUNT>-40.00</AMOUNT>",
            "<AMOUNT>-40.00</AMOUNT><BILLALLOCATIONS.LIST><NAME>Bill-1</NAME>\
             <BILLTYPE>New Ref</BILLTYPE><AMOUNT>-40.00</AMOUNT></BILLALLOCATIONS.LIST>",
        );
        let preview = store
            .preview_xml_import(company, with_bill.as_bytes())
            .unwrap();
        assert!(!preview.can_apply());
        assert!(preview
            .issues()
            .iter()
            .any(|issue| issue.contains("adjustment ledger must differ from the party")));

        let preview = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        assert_eq!(store.apply_xml_import(&preview).unwrap().vouchers, 1);
        assert!(store.invoices(company).unwrap().is_empty());
        let voucher = store.vouchers(company).unwrap().remove(0);
        assert_eq!(voucher.kind, VoucherKind::Sales);
        assert_eq!(voucher.party_lines, [0]);
        assert_eq!(voucher.postings[0].ledger_id, customer);
        assert_eq!(voucher.postings[0].amount, 4000);
        assert_eq!(voucher.postings[1].ledger_id, customer);

        assert!(store
            .update_voucher(
                company,
                voucher.id,
                &voucher.date,
                "Legacy edit",
                &voucher.postings
            )
            .is_err());
        assert!(store
            .voucher_revisions(company, voucher.id)
            .unwrap()
            .is_empty());
        let mut reordered = voucher.postings;
        reordered.rotate_left(1);
        store
            .update_voucher_draft(
                company,
                voucher.id,
                VoucherDraft {
                    kind: voucher.kind,
                    reference: &voucher.reference,
                    date: &voucher.date,
                    narration: "Reordered postings",
                    postings: &reordered,
                    party_lines: &[2],
                },
            )
            .unwrap();
        let revision = store
            .voucher_revisions(company, voucher.id)
            .unwrap()
            .remove(0);
        assert_eq!(revision.voucher.party_lines, [0]);
        assert_eq!(store.vouchers(company).unwrap()[0].party_lines, [2]);

        let reversal = store
            .reverse_voucher(company, voucher.id, "2026-01-03", "R-1", "Cancel sale")
            .unwrap();
        let counter = store
            .vouchers(company)
            .unwrap()
            .into_iter()
            .find(|row| row.id == reversal)
            .unwrap();
        assert_eq!(counter.party_lines, [2]);
        assert_eq!(counter.postings[2].ledger_id, customer);
        assert_eq!(counter.postings[2].amount, -4000);
        assert_eq!(
            store
                .ledger_register(company, customer, None, None)
                .unwrap()
                .closing_balance,
            0
        );
    }

    #[test]
    fn separate_master_and_transaction_exports_share_one_import() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let masters = br#"<?xml version="1.0"?>
            <ENVELOPE><BODY><DATA><MESSAGE>
                <LEDGER NAME="Trial Bank"><PARENT>Bank Accounts</PARENT>
                    <OPENINGBALANCE>-100.00</OPENINGBALANCE></LEDGER>
                <LEDGER NAME="Trial Earnings"><PARENT>Indirect Incomes</PARENT></LEDGER>
            </MESSAGE></DATA></BODY></ENVELOPE>"#;
        let vouchers = r#"<?xml version="1.0" encoding="UTF-16"?>
            <ENVELOPE><BODY><DATA><MESSAGE>
                <VOUCHER>
                    <VOUCHERTYPENAME>Journal</VOUCHERTYPENAME>
                    <DATE>20260102</DATE><VOUCHERNUMBER>J-01</VOUCHERNUMBER>
                    <ALLLEDGERENTRIES.LIST><LEDGERNAME>Trial Bank</LEDGERNAME>
                        <AMOUNT>-64.25</AMOUNT></ALLLEDGERENTRIES.LIST>
                    <ALLLEDGERENTRIES.LIST><LEDGERNAME>Trial Earnings</LEDGERNAME>
                        <AMOUNT>64.25</AMOUNT></ALLLEDGERENTRIES.LIST>
                </VOUCHER>
            </MESSAGE></DATA></BODY></ENVELOPE>"#;
        let utf16 = |xml: &str| {
            let mut bytes = vec![0xff, 0xfe];
            bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
            bytes
        };
        let good = utf16(vouchers);
        let unbalanced = utf16(&vouchers.replacen("-64.25", "-64.24", 1));

        assert!(!store
            .preview_xml_import(company, &good)
            .unwrap()
            .can_apply());
        let malformed = store
            .preview_xml_import_batch(company, &[masters, b"<broken"], Some("2026-01-01"))
            .unwrap_err();
        assert!(malformed.to_string().contains("XML file 2"));

        let blocked = store
            .preview_xml_import_batch(company, &[masters, &unbalanced], Some("2026-01-01"))
            .unwrap();
        assert_eq!(blocked.ledger_count(), 2);
        assert!(blocked
            .issues()
            .iter()
            .any(|issue| issue.contains("postings must balance")));
        assert!(store.apply_xml_import(&blocked).is_err());
        assert!(store.vouchers(company).unwrap().is_empty());
        assert!(!store
            .ledgers(company)
            .unwrap()
            .iter()
            .any(|ledger| ledger.name == "Trial Bank"));

        let preview = store
            .preview_xml_import_batch(company, &[masters, &good], Some("2026-01-01"))
            .unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        let counts = store.apply_xml_import(&preview).unwrap();
        assert_eq!(counts.ledgers, 2);
        assert_eq!(counts.vouchers, 1);
        let bank = store
            .ledgers(company)
            .unwrap()
            .into_iter()
            .find(|ledger| ledger.name == "Trial Bank")
            .unwrap();
        let activity = store
            .ledger_register(company, bank.id, Some("2026-01-02"), Some("2026-01-02"))
            .unwrap();
        assert_eq!(activity.opening_balance, 10000);
        assert_eq!(activity.entries[0].reference, "J-01");
        assert_eq!(activity.closing_balance, 16425);
    }

    #[test]
    fn import_checks_final_daily_stock_balances_before_committing() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Example").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let sales = store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let purchases = store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "Each").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        let date = "2026-01-02";
        store
            .set_negative_stock_policy(company, Some(date), "Keep stock nonnegative")
            .unwrap();
        let sale = ImportedInvoice {
            kind: InvoiceKind::Sale,
            entry_mode: InvoiceEntryMode::Invoice,
            date: date.into(),
            reference: "SELL-01".into(),
            bills: vec![BillSplitDraft {
                reference: "SELL-01".into(),
                amount: 3000,
                due_date: None,
                credit_days: None,
            }],
            advance_applications: Vec::new(),
            advance_bill_reference: None,
            narration: String::new(),
            party: "Customer".into(),
            existing_party_id: Some(customer),
            trade: "Sales".into(),
            existing_trade_id: Some(sales),
            adjustments: Vec::new(),
            original_postings: Vec::new(),
            metadata: SourceMetadata::default(),
            lines: vec![ImportedInvoiceLine {
                item: "Part".into(),
                existing_item_id: Some(item),
                unit: "Each".into(),
                precision: 3,
                trade_ledger: "Sales".into(),
                existing_trade_ledger_id: Some(sales),
                quantity: 3000,
                unit_price: 1000,
                discount: InvoiceDiscount::None,
                allocations: vec![ImportedStockAllocation {
                    location: "Main".into(),
                    existing_location_id: Some(location),
                    batch: None,
                    existing_batch_id: None,
                    quantity: 3000,
                }],
            }],
        };
        let mut purchase = sale.clone();
        purchase.kind = InvoiceKind::Purchase;
        purchase.reference = "BUY-01".into();
        purchase.bills[0].reference = "BUY-01".into();
        purchase.party = "Supplier".into();
        purchase.existing_party_id = Some(supplier);
        purchase.trade = "Purchases".into();
        purchase.existing_trade_id = Some(purchases);
        purchase.lines[0].trade_ledger = "Purchases".into();
        purchase.lines[0].existing_trade_ledger_id = Some(purchases);
        let mut plan = ImportPlan {
            company_id: company,
            opening_date: None,
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
            invoices: vec![sale],
            settlements: Vec::new(),
            issues: Vec::new(),
        };
        assert!(store
            .apply_xml_import(&plan)
            .unwrap_err()
            .to_string()
            .contains("Negative stock is blocked"));
        assert!(store.vouchers(company).unwrap().is_empty());
        plan.invoices.push(purchase);
        let counts = store.apply_xml_import(&plan).unwrap();
        assert_eq!(counts.invoices, 2);
        assert_eq!(
            store
                .stock_location_balances(company, location, Some(date))
                .unwrap()[0]
                .quantity,
            0
        );
    }
}
