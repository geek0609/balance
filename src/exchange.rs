//! Import of a limited accounting XML interchange format.
//! Unsupported or ambiguous data is reported before any database writes.
use crate::advances::{
    advance_allocation_state, insert_advance_allocation, insert_party_advance_parts,
    read_party_advances, AdvanceDraft, AdvanceSourceBill,
};
use crate::bill_note_refunds::{self, NoteRefundAllocation};
use crate::bill_splits::BillSplitDraft;
use crate::contacts::{insert_new_profile, LedgerProfile};
use crate::inventory::{
    check_quantity_precision, create_stock_batch_on_connection, create_stock_item_on_connection,
    discounted_line_amount_with_basis, find_stock_batch, insert_direct_invoice,
    insert_settlement_on_connection, insert_stock_adjustment, BillSettlementAllocation,
    BillSettlementTarget, InvoiceAdjustment, InvoiceAllocation, InvoiceDiscount, InvoiceDraft,
    InvoiceEntryMode, InvoiceExternalReference, InvoiceKind, InvoiceLine, InvoiceRateBasis,
    StockInvoicePosting,
};
use crate::opening_bills::OpeningBillDraft;
use crate::settlement_discounts::SettlementDiscount;
use crate::stock_units::{
    self, index_measure, resolve_compound, saved_measures, unit_key, CompoundUnit, UnitMeasure,
};
use crate::taxes::InvoiceTaxLine;
use crate::{
    check_new_bill_voucher, insert_voucher_draft, parse_amount, parse_date, Category, Error,
    Posting, Result, Store, VoucherDraft, VoucherKind,
};
use chrono::NaiveDate;
use roxmltree::{Document, Node, ParsingOptions};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::{prelude::ToPrimitive, Decimal, RoundingStrategy};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
mod cancelled;
mod orders;
mod stock_counts;
mod stock_journals;
mod stock_notes;
mod stock_rejections;
mod trade_defaults;
pub(crate) use stock_notes::migrate as migrate_stock_tracking;
pub(crate) use stock_rejections::migrate as migrate_stock_rejections;
pub(crate) use trade_defaults::migrate as migrate_trade_defaults;

/// Maximum combined input size for a single XML import preview.
pub const MAX_XML_IMPORT_BYTES: usize = 50 * 1024 * 1024;
const MAX_XML_NODES: u32 = 1_000_000;

pub(crate) fn migrate_voucher_keys(conn: &Connection) -> Result<()> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('imported_record_metadata') WHERE name = 'voucher_key')",
        [], |row| row.get(0),
    )?;
    if !present {
        conn.execute_batch(
            "ALTER TABLE imported_record_metadata ADD COLUMN voucher_key TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

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
    opening_references: Vec<ImportedOpeningReference>,
    credit_days: Option<u32>,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct ImportedOpeningReference {
    kind: InvoiceKind,
    reference: String,
    original_date: String,
    terms: BillTerms,
    amount: i64,
    advance_type: Option<&'static str>,
}
#[derive(Debug, Clone)]
struct Unit {
    symbol: String,
    formal_name: String,
    decimal_places: u8,
    compound: Option<CompoundUnit>,
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
    billed_quantity: i64,
    unit_price: i64,
    rate_basis: Option<InvoiceRateBasis>,
    discount: InvoiceDiscount,
}
#[derive(Debug, Clone)]
struct ImportedStockAllocation {
    location: String,
    existing_location_id: Option<i64>,
    batch: Option<String>,
    existing_batch_id: Option<i64>,
    quantity: i64,
    billed_quantity: i64,
    tracking: Option<String>,
}
#[derive(Debug, Clone)]
struct ImportedInvoiceAdjustment {
    ledger: String,
    existing_ledger_id: Option<i64>,
    amount: i64,
}

#[derive(Debug, Clone)]
struct ImportedInvoiceTax {
    ledger: String,
    existing_ledger_id: Option<i64>,
    amount: i64,
    source_rates: Vec<Decimal>,
}

#[derive(Debug, Clone)]
struct ImportedInvoice {
    kind: InvoiceKind,
    entry_mode: InvoiceEntryMode,
    date: String,
    source_order: usize,
    reference: String,
    external_reference: InvoiceExternalReference,
    bills: Vec<BillSplitDraft>,
    funding_applications: Vec<ImportedFundingApplication>,
    funded_bill_reference: Option<String>,
    narration: String,
    party: String,
    existing_party_id: Option<i64>,
    trade: String,
    existing_trade_id: Option<i64>,
    lines: Vec<ImportedInvoiceLine>,
    adjustments: Vec<ImportedInvoiceAdjustment>,
    taxes: Vec<ImportedInvoiceTax>,
    original_postings: Vec<Line>,
    metadata: SourceMetadata,
    order: Option<orders::InvoiceOrders>,
}
#[derive(Debug, Clone)]
struct ImportedFundingApplication {
    reference: String,
    amount: i64,
    terms: BillTerms,
    expected_source: Option<FundingSource>,
    expected_available: i128,
}

#[derive(Debug, Clone)]
struct ImportedBillAllocation {
    reference: String,
    amount: i64,
    existing_target: Option<BillSettlementTarget>,
    expected_outstanding: i128,
    terms: BillTerms,
}
#[derive(Debug, Clone)]
struct ImportedBillUse {
    invoices: Vec<ImportedBillAllocation>,
    advances: Vec<ImportedAdvance>,
}
#[derive(Debug, Clone)]
struct ImportedAdvance {
    amount: i64,
    source_bill: AdvanceSourceBill,
}
#[derive(Debug, Clone)]
struct ImportedSettlement {
    source_order: usize,
    kind: InvoiceKind,
    date: String,
    reference: String,
    narration: String,
    party: String,
    bank: String,
    existing_bank_id: Option<i64>,
    existing_party_id: Option<i64>,
    bills: ImportedBillUse,
    discounts: Vec<ImportedInvoiceAdjustment>,
    refunds: Vec<ImportedFundingApplication>,
    metadata: SourceMetadata,
}
impl ImportedSettlement {
    fn refund_total(&self) -> Result<i64> {
        self.refunds.iter().try_fold(0_i64, |total, refund| {
            total
                .checked_add(refund.amount)
                .ok_or_else(|| Error::Validation("Refund total is too large".into()))
        })
    }
}
#[derive(Debug, Clone)]
struct ImportedBillNote {
    source_order: usize,
    kind: InvoiceKind,
    date: String,
    reference: String,
    narration: String,
    lines: Vec<(Line, Option<i64>)>,
    party_index: usize,
    bills: Vec<ImportedBillAllocation>,
    credits: Vec<ImportedNoteCredit>,
    stock: Vec<ImportedInvoiceLine>,
    original_invoice: Option<OriginalReturnInvoice>,
    metadata: SourceMetadata,
}
#[derive(Debug, Clone)]
struct OriginalReturnInvoice {
    reference: String,
    date: Option<String>,
    existing_id: Option<i64>,
}

#[derive(Clone)]
struct StockInvoiceOrigin {
    voucher_number: String,
    voucher_date: String,
    external_reference: InvoiceExternalReference,
    existing_id: Option<i64>,
}

impl StockInvoiceOrigin {
    // Include case variants in ambiguity checks, then require exact source spelling.
    fn matching_spelling(&self, original: &OriginalReturnInvoice) -> Option<bool> {
        let key = normalized(&original.reference);
        [
            (
                Some(self.voucher_number.as_str()),
                Some(self.voucher_date.as_str()),
            ),
            (
                self.external_reference.number.as_deref(),
                self.external_reference.date.as_deref(),
            ),
        ]
        .into_iter()
        .filter_map(|(number, date)| {
            let number = number?;
            (normalized(number) == key
                && original
                    .date
                    .as_deref()
                    .is_none_or(|value| Some(value) == date))
            .then_some(number == original.reference)
        })
        .max()
    }
}
#[derive(Debug, Clone)]
struct ImportedNoteCredit {
    reference: String,
    amount: i64,
    terms: BillTerms,
}
impl ImportedBillNote {
    fn voucher_kind(&self) -> VoucherKind {
        match self.kind {
            InvoiceKind::Sale => VoucherKind::CreditNote,
            InvoiceKind::Purchase => VoucherKind::DebitNote,
        }
    }
}

enum BillChange {
    Settlement(usize),
    Note(usize),
}

impl BillChange {
    fn key<'a>(&self, plan: &'a ImportPlan) -> (&'a str, usize) {
        match *self {
            Self::Settlement(index) => {
                let row = &plan.settlements[index];
                (&row.date, row.source_order)
            }
            Self::Note(index) => {
                let row = &plan.bill_notes[index];
                (&row.date, row.source_order)
            }
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SourceMetadata {
    guid: String,
    remote_id: String,
    master_id: String,
    alteration_id: String,
    voucher_key: String,
}
impl SourceMetadata {
    fn is_empty(&self) -> bool {
        self.guid.is_empty()
            && self.remote_id.is_empty()
            && self.master_id.is_empty()
            && self.alteration_id.is_empty()
            && self.voucher_key.is_empty()
    }
    fn identities(&self) -> [(&'static str, &str); 4] {
        [
            ("guid", &self.guid),
            ("remote", &self.remote_id),
            ("master", &self.master_id),
            ("voucher key", &self.voucher_key),
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
    stock_notes: Vec<stock_notes::ImportedStockNote>,
    stock_returns: Vec<stock_rejections::ImportedRejection>,
    stock_counts: Vec<stock_counts::ImportedCount>,
    stock_journals: Vec<stock_journals::ImportedJournal>,
    cancelled_vouchers: Vec<cancelled::ImportedCancelled>,
    optional_vouchers: Vec<cancelled::ImportedOptional>,
    orders: Vec<orders::ImportedOrder>,
    stock_preview: Option<StockImportPreview>,
    settlements: Vec<ImportedSettlement>,
    bill_notes: Vec<ImportedBillNote>,
    issues: Vec<String>,
}
#[derive(Debug, Clone, Default)]
struct StockImportPreview {
    matches: Vec<stock_notes::TrackingMatch>,
    returns: Vec<stock_rejections::ReturnMatch>,
    counts: Vec<stock_counts::CountMatch>,
}

impl ImportPlan {
    fn check_period_lock(&self, conn: &Connection) -> Result<()> {
        let Some(cutoff) = crate::periods::cutoff(conn, self.company_id)? else {
            return Ok(());
        };
        let openings = self
            .accounts
            .iter()
            .any(|row| row.opening.is_some() || !row.opening_references.is_empty())
            || self
                .items
                .iter()
                .any(|item| item.openings.iter().any(|row| row.quantity != 0));
        let dates = self
            .transactions
            .iter()
            .map(|row| row.date.as_str())
            .chain(self.invoices.iter().map(|row| row.date.as_str()))
            .chain(self.stock_notes.iter().map(|row| row.date.as_str()))
            .chain(self.stock_returns.iter().map(|row| row.date.as_str()))
            .chain(self.stock_counts.iter().map(|row| row.date.as_str()))
            .chain(self.stock_journals.iter().map(|row| row.date.as_str()))
            .chain(self.orders.iter().map(|row| row.date.as_str()))
            .chain(self.settlements.iter().map(|row| row.date.as_str()))
            .chain(self.bill_notes.iter().map(|row| row.date.as_str()))
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
    pub fn stock_note_count(&self) -> usize {
        self.stock_notes.len()
    }
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }
    pub fn stock_return_count(&self) -> usize {
        self.stock_returns.len()
    }
    pub fn stock_count_count(&self) -> usize {
        self.stock_counts.len()
    }
    pub fn optional_voucher_count(&self) -> usize {
        self.optional_vouchers.len()
    }
    pub fn cancelled_voucher_count(&self) -> usize {
        self.cancelled_vouchers.len()
    }
    pub fn stock_journal_count(&self) -> usize {
        self.stock_journals.len()
    }
    pub fn stock_match_count(&self) -> usize {
        self.stock_preview
            .as_ref()
            .map_or(0, |preview| preview.matches.len())
    }
    pub fn settlement_count(&self) -> usize {
        self.settlements
            .iter()
            .filter(|row| !row.bills.invoices.is_empty())
            .count()
    }
    pub fn bill_note_count(&self) -> usize {
        self.bill_notes.len()
    }

    fn bill_changes(&self) -> Vec<BillChange> {
        let mut changes: Vec<_> = self
            .settlements
            .iter()
            .enumerate()
            .filter(|(_, settlement)| {
                !settlement.bills.invoices.is_empty() || !settlement.refunds.is_empty()
            })
            .map(|(index, row)| (&row.date, row.source_order, BillChange::Settlement(index)))
            .chain(
                self.bill_notes
                    .iter()
                    .enumerate()
                    .filter(|(_, note)| !note.bills.is_empty() || !note.stock.is_empty())
                    .map(|(index, row)| (&row.date, row.source_order, BillChange::Note(index))),
            )
            .collect();
        changes.sort_unstable_by_key(|(date, order, _)| (*date, *order));
        changes.into_iter().map(|(_, _, change)| change).collect()
    }
    pub fn advance_count(&self) -> usize {
        self.settlements
            .iter()
            .map(|row| row.bills.advances.len())
            .sum()
    }
    pub fn advance_application_count(&self) -> usize {
        self.invoices
            .iter()
            .flat_map(|row| &row.funding_applications)
            .filter(|application| {
                matches!(application.expected_source, Some(FundingSource::Advance(_)))
            })
            .count()
    }
    pub fn credit_application_count(&self) -> usize {
        self.invoices
            .iter()
            .flat_map(|row| &row.funding_applications)
            .filter(|application| {
                matches!(application.expected_source, Some(FundingSource::Note(_)))
            })
            .count()
    }
    pub fn note_credit_count(&self) -> usize {
        self.bill_notes.iter().map(|row| row.credits.len()).sum()
    }
    pub fn note_refund_count(&self) -> usize {
        self.settlements.iter().map(|row| row.refunds.len()).sum()
    }
    pub fn opening_count(&self) -> usize {
        self.accounts
            .iter()
            .filter(|a| a.opening.is_some() && a.opening_references.is_empty())
            .count()
    }
    pub fn opening_bill_count(&self) -> usize {
        self.accounts
            .iter()
            .flat_map(|a| &a.opening_references)
            .filter(|bill| bill.advance_type.is_none())
            .count()
    }
    pub fn opening_advance_count(&self) -> usize {
        self.accounts
            .iter()
            .flat_map(|a| &a.opening_references)
            .filter(|bill| bill.advance_type.is_some())
            .count()
    }
    pub fn opening_on_account_count(&self) -> usize {
        self.accounts
            .iter()
            .filter(|a| a.opening.is_some() && !a.opening_references.is_empty())
            .count()
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
                || !self.stock_notes.is_empty()
                || !self.stock_returns.is_empty()
                || !self.stock_counts.is_empty()
                || !self.stock_journals.is_empty()
                || !self.cancelled_vouchers.is_empty()
                || !self.optional_vouchers.is_empty()
                || !self.orders.is_empty()
                || !self.settlements.is_empty()
                || !self.bill_notes.is_empty())
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ImportCounts {
    pub groups: usize,
    pub ledgers: usize,
    pub opening_bills: usize,
    pub opening_advances: usize,
    pub opening_on_account: usize,
    pub vouchers: usize,
    pub invoices: usize,
    pub stock_notes: usize,
    pub stock_returns: usize,
    pub stock_counts: usize,
    pub stock_journals: usize,
    pub cancelled_vouchers: usize,
    pub optional_vouchers: usize,
    pub orders: usize,
    pub stock_matches: usize,
    pub settlements: usize,
    pub bill_notes: usize,
    pub advances: usize,
    pub advance_applications: usize,
    pub note_credits: usize,
    pub credit_applications: usize,
    pub note_refunds: usize,
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
    let voucher = local_name(node).eq_ignore_ascii_case("VOUCHER");
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
        let metadata = (voucher
            && (tag.eq_ignore_ascii_case("VCHKEY") || cancelled::status_field(tag)))
            || record
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
            && !(voucher
                && ["DATE", "VCHTYPE", "OBJVIEW", "VCHKEY"]
                    .iter()
                    .any(|a| attribute.name().eq_ignore_ascii_case(a)))
            && !(voucher && cancelled::status_field(attribute.name()))
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
        let mut attributes = node
            .attributes()
            .filter(|a| a.name().eq_ignore_ascii_case(name));
        let attribute = attributes
            .next()
            .map(|a| a.value().trim())
            .unwrap_or_default();
        if attributes.next().is_some() {
            issues.push(format!("{label}: attribute {name} occurs more than once"));
        }
        if !child.is_empty() && !attribute.is_empty() && child != attribute {
            issues.push(format!("{label}: conflicting {name} values"));
        }
        if child.is_empty() {
            attribute.to_owned()
        } else {
            child
        }
    };
    SourceMetadata {
        guid: value("GUID", issues),
        remote_id: value("REMOTEID", issues),
        master_id: value("MASTERID", issues),
        alteration_id: value("ALTERID", issues),
        voucher_key: if local_name(node).eq_ignore_ascii_case("VOUCHER") {
            value("VCHKEY", issues)
        } else {
            String::new()
        },
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
        if value.is_empty() {
            continue;
        }
        let value = normalized(value);
        if cancelled::other_status_seen(kind, key, &value, seen) {
            issues.push(format!(
                "{label}: a source voucher with this {key} already has a different posting status"
            ));
        }
        if !seen.insert((kind.to_owned(), key.to_owned(), value)) {
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
    let existing = conn.query_row(
        "SELECT guid, remote_id, master_id, alteration_id, voucher_key FROM imported_record_metadata
         WHERE company_id = ?1 AND kind = ?2 AND local_id = ?3",
        params![company, kind, local_id],
        |r| Ok(SourceMetadata {
            guid: r.get(0)?, remote_id: r.get(1)?, master_id: r.get(2)?, alteration_id: r.get(3)?, voucher_key: r.get(4)?,
        }),
    ).optional()?;
    if let Some(existing) = existing {
        if *meta == existing {
            return Ok(());
        }
        return Err(Error::Validation(
            "Imported identifiers changed since preview".into(),
        ));
    }
    let mut stmt = conn.prepare_cached(
        "SELECT kind, guid, remote_id, master_id, voucher_key FROM imported_record_metadata
         WHERE company_id = ?1 AND (
             kind = ?2 OR (?3 AND kind IN (?4, ?5))
             OR (?6 AND kind IN ('voucher', 'stocknote', 'order', 'stockreturn', 'stockcount', 'stockjournal'))
         )",
    )?;
    let rows = stmt.query_map(
        params![
            company,
            kind,
            cancelled::is_posted_kind(kind) || cancelled::is_inactive_kind(kind),
            cancelled::KIND,
            cancelled::OPTIONAL_KIND,
            cancelled::is_inactive_kind(kind)
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                [
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ],
            ))
        },
    )?;
    let identities = meta
        .identities()
        .map(|(key, value)| (key, normalized(value)));
    for row in rows {
        let (saved_kind, saved_values) = row?;
        if identities
            .iter()
            .zip(saved_values)
            .any(|((key, value), saved)| {
                !value.is_empty()
                    && (saved_kind == kind || *key != "master")
                    && *value == normalized(&saved)
            })
        {
            if saved_kind != kind {
                return Err(Error::Validation(
                    "A source voucher already exists with a different posting status".into(),
                ));
            }
            return Err(Error::Validation(
                "An imported identifier already exists".into(),
            ));
        }
    }
    drop(stmt);
    conn.execute("INSERT INTO imported_record_metadata(company_id, kind, local_id, guid, remote_id, master_id, alteration_id, voucher_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![company, kind, local_id, meta.guid, meta.remote_id, meta.master_id, meta.alteration_id, meta.voucher_key])?;
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
pub(crate) fn parse_kind(value: &str) -> Option<VoucherKind> {
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
fn stock_opening_cost(
    node: Node<'_, '_>,
    quantity: i64,
    unit: &UnitMeasure,
) -> Result<Option<i64>> {
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
            let factor = unit.factor_for(suffix.unwrap_or(""))?;
            let rate = Decimal::from_str(number.trim())
                .map_err(|_| Error::Validation("opening rate is invalid".into()))?;
            if rate < Decimal::ZERO || (!rate.is_zero() && rate.scale() > 5) {
                return Err(Error::Validation(
                    "opening rate must be non-negative with at most five decimal places".into(),
                ));
            }
            Ok((rate, factor))
        })
        .transpose()?;
    if value.is_none_or(|value| value == 0) && rate.is_none_or(|(rate, _)| rate.is_zero()) {
        // Legacy files use zero fields for quantity-only openings.
        return Ok(None);
    }
    if quantity <= 0 {
        return Err(Error::Validation(
            "opening valuation requires a positive quantity".into(),
        ));
    }
    let nonzero_rate = rate.filter(|(rate, _)| !rate.is_zero());
    if value.is_some_and(|value| value < 0) && nonzero_rate.is_none() {
        return Err(Error::Validation(
            "a negative opening value needs a matching positive rate".into(),
        ));
    }
    let too_large = || Error::Validation("opening value is too large".into());
    let extended = |rate, factor| {
        Decimal::new(quantity, 3)
            .checked_mul(rate)
            .and_then(|amount| amount.checked_div(factor))
            .ok_or_else(too_large)
    };
    if let Some(value) = value {
        let value = value.checked_abs().ok_or_else(too_large)?;
        if let Some((rate, factor)) = nonzero_rate {
            let calculated = extended(rate, factor)?;
            // A total may be more precise than the displayed rate. Accept only
            // amounts consistent with half a last-place rate unit plus rounding.
            let tolerance = extended(Decimal::new(5, rate.scale() + 1), factor)?
                .checked_add(Decimal::new(5, 3))
                .ok_or_else(too_large)?;
            if (Decimal::new(value, 2) - calculated).abs() > tolerance {
                return Err(Error::Validation(
                    "opening value conflicts with quantity and rate".into(),
                ));
            }
        }
        return Ok(Some(value));
    }
    let Some((rate, factor)) = nonzero_rate else {
        return Ok(None);
    };
    let minor_rate = rate.checked_mul(Decimal::from(100)).ok_or_else(too_large)?;
    let minor = extended(minor_rate, factor)?
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_i64()
        .ok_or_else(too_large)?;
    Ok(Some(minor))
}

fn opening_cost_or_issue(
    node: Node<'_, '_>,
    quantity: i64,
    unit: &UnitMeasure,
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

fn xml_stock_location(
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
#[derive(Debug, Clone, Default)]
struct BillTerms {
    due_date: Option<String>,
    credit_days: Option<u32>,
}

impl BillTerms {
    fn is_specified(&self) -> bool {
        self.due_date.is_some() || self.credit_days.is_some()
    }

    fn due_date_from(&self, date: NaiveDate) -> Result<Option<NaiveDate>> {
        bill_due_date(date, self.due_date.as_deref(), self.credit_days)
    }

    fn matches_due_date(&self, original: NaiveDate, due: Option<NaiveDate>) -> Result<bool> {
        Ok(!self.is_specified() || self.due_date_from(original)? == due)
    }
}

fn bill_due_date(
    original: NaiveDate,
    due_date: Option<&str>,
    credit_days: Option<u32>,
) -> Result<Option<NaiveDate>> {
    if let Some(date) = due_date {
        return parse_date(date).map(Some);
    }
    credit_days
        .map(|days| {
            original
                .checked_add_days(chrono::Days::new(u64::from(days)))
                .ok_or_else(|| {
                    Error::Validation("Credit period produces an invalid due date".into())
                })
        })
        .transpose()
}

fn xml_bill_terms(value: Option<&str>) -> Result<BillTerms> {
    let Some(value) = value else {
        return Ok(BillTerms::default());
    };
    if let Ok(date) = xml_date(value) {
        return Ok(BillTerms {
            due_date: Some(date),
            credit_days: None,
        });
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%d-%b-%Y") {
        return Ok(BillTerms {
            due_date: Some(date.to_string()),
            credit_days: None,
        });
    }
    Ok(BillTerms {
        due_date: None,
        credit_days: Some(xml_credit_days(value)?),
    })
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
fn xml_opening_reference(
    node: Node<'_, '_>,
    kind: InvoiceKind,
    opening_date: NaiveDate,
) -> Result<ImportedOpeningReference> {
    let reference = invoice_field(node, "NAME")?;
    let advance_flag =
        match normalized(&direct_text(node, "ISADVANCE").unwrap_or_default()).as_str() {
            "" => None,
            "yes" => Some(true),
            "no" => Some(false),
            _ => return Err(Error::Validation("Invalid opening advance flag".into())),
        };
    let original_date = xml_date(&invoice_field(node, "BILLDATE")?)?;
    let original = parse_date(&original_date)?;
    if original > opening_date {
        return Err(Error::Validation(
            "Bill date follows the opening date".into(),
        ));
    }
    let bill_type = normalized(&direct_text(node, "BILLTYPE").unwrap_or_default());
    let advance_type = match (bill_type.as_str(), advance_flag) {
        ("", Some(true)) | ("advance", None | Some(true)) => Some("Advance"),
        ("new ref", Some(true)) => Some("New Ref"),
        ("" | "new ref", _) => None,
        _ => {
            return Err(Error::Validation(
                "Invalid type or advance flag for an opening reference".into(),
            ))
        }
    };
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
    if signed_amount == 0 || matching_side == advance_type.is_some() {
        return Err(Error::Validation(
            "Opening reference has a zero amount or the wrong debit or credit side".into(),
        ));
    }
    let amount = signed_amount
        .checked_abs()
        .ok_or_else(|| Error::Validation("Opening bill amount is too large".into()))?;
    let credit = direct_text(node, "BILLCREDITPERIOD").filter(|value| !value.is_empty());
    let terms = xml_bill_terms(credit.as_deref())?;
    if terms
        .due_date_from(original)?
        .is_some_and(|due| due < original)
    {
        return Err(Error::Validation(
            "Opening bill due date precedes its original date".into(),
        ));
    }
    Ok(ImportedOpeningReference {
        kind,
        reference,
        original_date,
        terms,
        amount,
        advance_type,
    })
}

fn opening_references(
    node: Node<'_, '_>,
    ledger_amount: Option<i64>,
    category: Category,
    opening_date: Option<&str>,
    label: &str,
    issues: &mut Vec<String>,
) -> Vec<ImportedOpeningReference> {
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
        (Category::Asset, Some(_)) => InvoiceKind::Sale,
        (Category::Liability, Some(_)) => InvoiceKind::Purchase,
        _ => {
            issues.push(format!("{label}: opening bills require a declared opening balance on an asset or liability ledger"));
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
                "ISADVANCE",
                "OPENINGBALANCE",
                "AMOUNT",
                "BILLCREDITPERIOD",
            ],
            label,
            issues,
        );
        match xml_opening_reference(node, kind, date) {
            Ok(bill) if references.insert(normalized(&bill.reference)) => bills.push(bill),
            Ok(bill) => issues.push(format!(
                "{label}: duplicate opening bill reference {}",
                bill.reference
            )),
            Err(error) => issues.push(format!("{label}: opening bill: {error}")),
        }
    }
    bills
}

struct BilledVoucher<'a, 'input> {
    kind: VoucherKind,
    date: String,
    reference: String,
    entries: Vec<Node<'a, 'input>>,
}

const RETURN_REFERENCE_FIELDS: [&str; 4] =
    ["REFERENCE", "REF", "ORIGINALINVOICENUMBER", "ORIGINALINVNO"];
const RETURN_DATE_FIELDS: [&str; 4] = [
    "REFERENCEDATE",
    "REFDATE",
    "ORIGINALINVOICEDATE",
    "ORIGINALINVDATE",
];
const INVOICE_REFERENCE_FIELDS: [&str; 2] = ["REFERENCE", "REF"];
const INVOICE_REFERENCE_DATE_FIELDS: [&str; 2] = ["REFERENCEDATE", "REFDATE"];

fn invoice_document_reference(
    node: Node<'_, '_>,
    number_fields: &[&str],
    date_fields: &[&str],
) -> Result<InvoiceExternalReference> {
    let mut references = number_fields
        .iter()
        .filter_map(|field| direct_text(node, field))
        .filter(|value| !value.is_empty());
    let number = references.next();
    if references.any(|value| Some(&value) != number.as_ref()) {
        return Err(Error::Validation(
            "Conflicting invoice reference numbers".into(),
        ));
    }
    let mut dates = date_fields
        .iter()
        .filter_map(|field| direct_text(node, field))
        .filter(|value| !value.is_empty())
        .map(|value| xml_date(&value));
    let date = dates.next().transpose()?;
    for value in dates {
        if Some(value?) != date {
            return Err(Error::Validation(
                "Conflicting invoice reference dates".into(),
            ));
        }
    }
    Ok(InvoiceExternalReference { number, date })
}

fn original_return_invoice(node: Node<'_, '_>) -> Result<Option<OriginalReturnInvoice>> {
    let InvoiceExternalReference { number, date } =
        invoice_document_reference(node, &RETURN_REFERENCE_FIELDS, &RETURN_DATE_FIELDS)?;
    if number.is_none() && date.is_some() {
        return Err(Error::Validation(
            "Original invoice date requires its reference".into(),
        ));
    }
    Ok(number.map(|reference| OriginalReturnInvoice {
        reference,
        date,
        existing_id: None,
    }))
}

fn billed_voucher<'a, 'input>(
    node: Node<'a, 'input>,
    label: &str,
    issues: &mut Vec<String>,
) -> Result<BilledVoucher<'a, 'input>> {
    let voucher_kind = parse_kind(&invoice_field(node, "VOUCHERTYPENAME")?)
        .ok_or_else(|| Error::Validation("unsupported voucher type".into()))?;
    let is_note = matches!(
        voucher_kind,
        VoucherKind::CreditNote | VoucherKind::DebitNote
    );
    let mut fields = vec![
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
    ];
    if is_note {
        fields.extend(RETURN_REFERENCE_FIELDS);
        fields.extend(RETURN_DATE_FIELDS);
    }
    check_fields(node, &fields, label, issues);
    if node
        .attributes()
        .any(|a| a.name().eq_ignore_ascii_case("NAME") && !a.value().trim().is_empty())
    {
        issues.push(format!("{label}: voucher name attribute is not supported"));
    }
    if !is_note
        && node.children().any(|row| {
            row.is_element()
                && local_name(row).eq_ignore_ascii_case("ALLINVENTORYENTRIES.LIST")
                && meaningful(row)
        })
    {
        return Err(Error::Validation(
            "Stock belongs on an inventory invoice or return".into(),
        ));
    }
    let views: &[&str] = if is_note {
        &["Accounting Voucher View", "Invoice Voucher View"]
    } else {
        &["Accounting Voucher View"]
    };
    check_voucher_view(node, views)?;
    if direct_text(node, "ISINVOICE").is_some_and(|v| {
        !(v.is_empty()
            || v.eq_ignore_ascii_case("no")
            || (is_note && v.eq_ignore_ascii_case("yes")))
    }) {
        return Err(Error::Validation(
            "unsupported invoice mode for these bill allocations".into(),
        ));
    }
    if let Some(attr) = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("VCHTYPE"))
    {
        if parse_kind(attr.value()) != Some(voucher_kind) {
            return Err(Error::Validation("conflicting voucher type".into()));
        }
    }
    let date = xml_date(&invoice_field(node, "DATE")?)?;
    if let Some(attr) = node
        .attributes()
        .find(|a| a.name().eq_ignore_ascii_case("DATE"))
    {
        if xml_date(attr.value())? != date {
            return Err(Error::Validation("conflicting voucher date".into()));
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
    Ok(BilledVoucher {
        kind: voucher_kind,
        date,
        reference,
        entries,
    })
}

fn parse_imported_settlement(
    node: Node<'_, '_>,
    label: &str,
    source_order: usize,
    metadata: SourceMetadata,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    issues: &mut Vec<String>,
) -> Result<ImportedSettlement> {
    let BilledVoucher {
        kind: voucher_kind,
        date,
        reference,
        entries,
    } = billed_voucher(node, label, issues)?;
    let kind = match voucher_kind {
        VoucherKind::Receipt => InvoiceKind::Sale,
        VoucherKind::Payment => InvoiceKind::Purchase,
        _ => {
            return Err(Error::Validation(
                "bill allocations require a receipt or payment".into(),
            ))
        }
    };
    let mut party: Option<(String, Option<i64>, i64, ImportedBillUse)> = None;
    let mut bank: Option<(String, Option<i64>, i64)> = None;
    let mut discounts = Vec::new();
    let mut entry_ledgers = HashSet::new();
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
        if !entry_ledgers.insert(normalized(&ledger)) {
            return Err(Error::Validation(
                "a settlement ledger occurs more than once".into(),
            ));
        }
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
            match category {
                Category::Asset if bank.is_none() => bank = Some((ledger, *existing_id, amount)),
                Category::Income | Category::Expense => {
                    if (amount > 0) != (kind == InvoiceKind::Sale) {
                        return Err(Error::Validation("discount direction differs from the cash posting".into()));
                    }
                    discounts.push(ImportedInvoiceAdjustment {
                        ledger,
                        existing_ledger_id: *existing_id,
                        amount: amount.checked_abs().ok_or_else(|| Error::Validation("discount amount is too large".into()))?,
                    });
                }
                _ => return Err(Error::Validation(
                    "settlement needs one cash or bank asset ledger and income or expense discount ledgers".into(),
                )),
            }
        } else {
            if party.is_some() || !matches!(category, Category::Asset | Category::Liability) {
                return Err(Error::Validation(
                    "settlement needs one party ledger".into(),
                ));
            }
            let mut allocations = Vec::new();
            let mut advances = Vec::new();
            let mut advance_references = HashSet::new();
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
                if matches!(bill_type.as_str(), "advance" | "new ref" | "on account") {
                    let source = match bill_type.as_str() {
                        "advance" => "Advance",
                        "new ref" => "New Ref",
                        _ => "On Account",
                    };
                    let reference = direct_text(bill, "NAME").filter(|value| !value.is_empty());
                    if source != "On Account" && reference.is_none() {
                        return Err(Error::Validation(
                            "a named advance needs a bill reference".into(),
                        ));
                    }
                    if source != "On Account"
                        && reference
                            .as_deref()
                            .is_some_and(|name| !advance_references.insert(normalized(name)))
                    {
                        return Err(Error::Validation(
                            "A named advance reference occurs more than once".into(),
                        ));
                    }
                    let terms = xml_bill_terms(credit.as_deref())?;
                    if source == "On Account" && terms.is_specified() {
                        return Err(Error::Validation(
                            "on-account allocations cannot carry credit terms".into(),
                        ));
                    }
                    terms.due_date_from(parse_date(&date)?)?;
                    advances.push(ImportedAdvance {
                        amount: value,
                        source_bill: AdvanceSourceBill {
                            kind: source.into(),
                            reference,
                            due_date: terms.due_date,
                            credit_days: terms.credit_days,
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
                    terms: xml_bill_terms(credit.as_deref())?,
                });
            }
            let sum = allocations
                .iter()
                .map(|v| i128::from(v.amount))
                .sum::<i128>()
                + advances.iter().map(|v| i128::from(v.amount)).sum::<i128>();
            if sum != i128::from(amount).abs() {
                return Err(Error::Validation(
                    "bill allocations do not equal the party amount".into(),
                ));
            }
            let bills = ImportedBillUse {
                invoices: allocations,
                advances,
            };
            party = Some((ledger, *existing_id, amount, bills));
        }
    }
    let (party, existing_party_id, party_amount, bills) =
        party.ok_or_else(|| Error::Validation("bill allocations are missing".into()))?;
    let (bank, existing_bank_id, bank_amount) =
        bank.ok_or_else(|| Error::Validation("cash or bank ledger is missing".into()))?;
    let discounted = discounts.iter().map(|d| i128::from(d.amount)).sum::<i128>();
    let settled = bills
        .invoices
        .iter()
        .map(|bill| i128::from(bill.amount))
        .sum::<i128>();
    if discounted > settled {
        return Err(Error::Validation(
            "discounts exceed the amount allocated to bills".into(),
        ));
    }
    if i128::from(party_amount).abs() != i128::from(bank_amount).abs() + discounted
        || (bank_amount > 0) != (kind == InvoiceKind::Sale)
        || (party_amount > 0) == (bank_amount > 0)
    {
        return Err(Error::Validation(
            "party, cash and discount amounts or directions are inconsistent".into(),
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
        source_order,
        kind,
        date,
        reference,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
        party,
        bank,
        existing_bank_id,
        existing_party_id,
        bills,
        discounts,
        refunds: Vec::new(),
        metadata,
    })
}

fn parse_imported_bill_note(
    node: Node<'_, '_>,
    label: &str,
    source_order: usize,
    metadata: SourceMetadata,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    items: &InvoiceStockItems,
    locations: &HashMap<String, Option<i64>>,
    batches: &HashMap<(String, String), i64>,
    issues: &mut Vec<String>,
) -> Result<ImportedBillNote> {
    let BilledVoucher {
        kind,
        date,
        reference,
        entries,
    } = billed_voucher(node, label, issues)?;
    let kind = match kind {
        VoucherKind::CreditNote => InvoiceKind::Sale,
        VoucherKind::DebitNote => InvoiceKind::Purchase,
        _ => return Err(Error::Validation("expected a credit or debit note".into())),
    };
    let mut inventory_rows = node
        .children()
        .filter(|row| {
            row.is_element()
                && local_name(*row).eq_ignore_ascii_case("ALLINVENTORYENTRIES.LIST")
                && meaningful(*row)
        })
        .map(|row| (row, None))
        .collect::<Vec<_>>();
    let header_inventory = !inventory_rows.is_empty();
    let mut breakdowns = Vec::new();
    let mut lines = Vec::with_capacity(entries.len());
    let mut allocations = Vec::new();
    let mut credits = Vec::new();
    let mut party = None;
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
        let (existing_id, category) = ledgers
            .get(&normalized(&ledger))
            .ok_or_else(|| Error::Validation(format!("ledger {ledger} is unavailable")))?;
        let amount = xml_amount(&invoice_field(entry, "AMOUNT")?)?;
        if amount == 0 {
            return Err(Error::Validation("zero ledger amount".into()));
        }
        invoice_side(entry, "ISDEEMEDPOSITIVE", amount > 0)?;
        invoice_side(entry, "ISLASTDEEMEDPOSITIVE", amount > 0)?;
        let mut is_party = party_flag(entry)?;
        let bills: Vec<_> = entry
            .children()
            .filter(|bill| {
                bill.is_element()
                    && local_name(*bill).eq_ignore_ascii_case("BILLALLOCATIONS.LIST")
                    && meaningful(*bill)
            })
            .collect();
        let stock = entry
            .children()
            .filter(|row| {
                row.is_element()
                    && local_name(*row).eq_ignore_ascii_case("INVENTORYALLOCATIONS.LIST")
                    && meaningful(*row)
            })
            .collect::<Vec<_>>();
        if !stock.is_empty() {
            if header_inventory || is_party || !bills.is_empty() {
                return Err(Error::Validation(
                    "Return stock must use one inventory representation on trade lines".into(),
                ));
            }
            let index = breakdowns.len();
            inventory_rows.extend(stock.into_iter().map(|row| (row, Some(index))));
            breakdowns.push(StockLedgerBreakdown {
                ledger: ledger.clone(),
                posting_amount: amount,
                stock_amount: 0,
            });
        }
        if !bills.is_empty() {
            if party.is_some() || !matches!(category, Category::Asset | Category::Liability) {
                return Err(Error::Validation(
                    "note allocations must belong to one party ledger".into(),
                ));
            }
            if (amount < 0) != (kind == InvoiceKind::Sale) {
                return Err(Error::Validation(
                    "note direction must reduce the amount owed on the bill".into(),
                ));
            }
            let mut seen = HashSet::new();
            for bill in bills {
                invoice_list_fields(
                    bill,
                    &["NAME", "BILLTYPE", "AMOUNT", "BILLCREDITPERIOD"],
                    label,
                    issues,
                );
                let bill_type = normalized(&invoice_field(bill, "BILLTYPE")?);
                let bill_reference = invoice_field(bill, "NAME")?;
                if !seen.insert(normalized(&bill_reference)) {
                    return Err(Error::Validation(
                        "bill reference occurs more than once".into(),
                    ));
                }
                let bill_amount = xml_amount(&invoice_field(bill, "AMOUNT")?)?;
                if bill_amount.signum() != amount.signum() {
                    return Err(Error::Validation(
                        "bill allocation direction is inconsistent".into(),
                    ));
                }
                let amount = bill_amount
                    .checked_abs()
                    .ok_or_else(|| Error::Validation("bill amount is too large".into()))?;
                let terms = xml_bill_terms(direct_text(bill, "BILLCREDITPERIOD").as_deref())?;
                match bill_type.as_str() {
                    "agst ref" => allocations.push(ImportedBillAllocation {
                        reference: bill_reference,
                        amount,
                        terms,
                        existing_target: None,
                        expected_outstanding: 0,
                    }),
                    "new ref" => {
                        terms.due_date_from(parse_date(&date)?)?;
                        credits.push(ImportedNoteCredit {
                            reference: bill_reference,
                            amount,
                            terms,
                        });
                    }
                    _ => {
                        return Err(Error::Validation(
                            "note needs an existing bill or a new named credit reference".into(),
                        ))
                    }
                }
            }
            if allocations
                .iter()
                .map(|bill| i128::from(bill.amount))
                .chain(credits.iter().map(|credit| i128::from(credit.amount)))
                .sum::<i128>()
                != i128::from(amount).abs()
            {
                return Err(Error::Validation(
                    "bill allocations do not equal the party amount".into(),
                ));
            }
            is_party = true;
            party = Some(lines.len());
        }
        lines.push((
            Line {
                ledger,
                amount,
                is_party,
            },
            *existing_id,
        ));
    }
    let party_index =
        party.ok_or_else(|| Error::Validation("note bill allocations are missing".into()))?;
    let party = &lines[party_index].0.ledger;
    if direct_text(node, "PARTYLEDGERNAME")
        .is_some_and(|name| !name.is_empty() && normalized(&name) != normalized(party))
    {
        return Err(Error::Validation(
            "party ledger differs from the allocated ledger".into(),
        ));
    }
    if lines.iter().enumerate().any(|(index, (line, _))| {
        index != party_index && normalized(&line.ledger) == normalized(party)
    }) {
        return Err(Error::Validation(
            "party posting must carry all of the note's bill allocations".into(),
        ));
    }
    if lines
        .iter()
        .map(|(line, _)| i128::from(line.amount))
        .sum::<i128>()
        != 0
    {
        return Err(Error::Validation("note postings do not balance".into()));
    }
    let (mut stock, stock_total) = parse_invoice_stock(
        inventory_rows,
        breakdowns,
        kind,
        true,
        label,
        ledgers,
        items,
        locations,
        batches,
        issues,
    )?;
    if stock.iter().any(|line| line.trade_ledger.is_empty()) {
        let trade = stock
            .iter()
            .find(|line| !line.trade_ledger.is_empty())
            .map(|line| (line.trade_ledger.clone(), line.existing_trade_ledger_id))
            .ok_or_else(|| {
                Error::Validation("A zero-value return item needs an accounting ledger".into())
            })?;
        for line in stock.iter_mut().filter(|line| line.trade_ledger.is_empty()) {
            line.trade_ledger.clone_from(&trade.0);
            line.existing_trade_ledger_id = trade.1;
        }
    }
    let original_invoice = original_return_invoice(node)?;
    if stock.is_empty() && original_invoice.is_some() {
        return Err(Error::Validation(
            "Original invoice references on financial notes require stock details".into(),
        ));
    }
    if !stock.is_empty() && stock_total > i128::from(lines[party_index].0.amount).abs() {
        return Err(Error::Validation(
            "Return item amounts exceed the note amount".into(),
        ));
    }
    if !stock.is_empty() && allocations.is_empty() && original_invoice.is_none() {
        return Err(Error::Validation(
            "A stock return with unused credit needs the original invoice reference".into(),
        ));
    }
    Ok(ImportedBillNote {
        source_order,
        kind,
        date,
        reference,
        party_index,
        lines,
        bills: allocations,
        credits,
        stock,
        original_invoice,
        metadata,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
    })
}
const INVOICE_TAX_RATE_FIELD: &str = "BASICRATEOFINVOICETAX";
const INVOICE_TAX_RATE_LIST: &str = "BASICRATEOFINVOICETAX.LIST";

fn xml_invoice_tax_rates(entry: Node<'_, '_>) -> Result<Vec<Decimal>> {
    let mut rates = Vec::new();
    let numeric_attributes = |node: Node<'_, '_>| {
        node.attributes().all(|attribute| {
            attribute.name().eq_ignore_ascii_case("TYPE")
                && attribute.value().eq_ignore_ascii_case("Number")
        })
    };
    for list in entry.children().filter(|node| {
        node.is_element() && local_name(*node).eq_ignore_ascii_case(INVOICE_TAX_RATE_LIST)
    }) {
        if !numeric_attributes(list)
            || list.children().any(|node| {
                node.is_text() && node.text().is_some_and(|text| !text.trim().is_empty())
            })
        {
            return Err(Error::Validation(
                "Invoice tax rates must be a list of numbers".into(),
            ));
        }
        for field in list.children().filter(|node| node.is_element()) {
            if !local_name(field).eq_ignore_ascii_case(INVOICE_TAX_RATE_FIELD)
                || !numeric_attributes(field)
                || field.children().any(|node| node.is_element())
            {
                return Err(Error::Validation(
                    "Invalid field in invoice tax rate list".into(),
                ));
            }
            let value: String = field
                .children()
                .filter(|node| node.is_text())
                .filter_map(|node| node.text())
                .collect();
            if !value.trim().is_empty() {
                rates.push(Decimal::from_str_exact(value.trim()).map_err(|_| {
                    Error::Validation("Invoice tax rate must be a decimal number".into())
                })?);
            }
        }
    }
    Ok(rates)
}

/// Parse only invoices whose party, accounting, amounts and bill reference
/// can all be represented without dropping information.
fn parse_imported_invoice(
    node: Node<'_, '_>,
    source_order: usize,
    label: &str,
    metadata: SourceMetadata,
    entry_mode: InvoiceEntryMode,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    items: &InvoiceStockItems,
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
            "BASICORDERREF",
            "ORDERNO",
            "ORDERDUEDATE",
            "REFERENCE",
            "REF",
            "REFERENCEDATE",
            "REFDATE",
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
    let mut taxes = Vec::new();
    let mut tax_ledgers = HashSet::new();
    let mut zero_trade_ledgers = HashSet::new();
    let mut original_postings = Vec::new();
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
                INVOICE_TAX_RATE_LIST,
            ],
            label,
            issues,
        );
        let ledger = invoice_field(entry, "LEDGERNAME")?;
        let (existing_ledger_id, category) = ledgers
            .get(&normalized(&ledger))
            .ok_or_else(|| Error::Validation(format!("ledger {ledger} is unavailable")))?;
        let amount = xml_item_amount(entry)?;
        if amount != 0 {
            invoice_side(entry, "ISDEEMEDPOSITIVE", amount > 0)?;
            invoice_side(entry, "ISLASTDEEMEDPOSITIVE", amount > 0)?;
        }
        let named_party = header_party
            .as_ref()
            .is_some_and(|name| normalized(name) == normalized(&ledger));
        let is_party = party_flag(entry)?
            || (named_party
                && direct_text(entry, "ISPARTYLEDGER").is_none_or(|value| value.is_empty()));
        if entry_mode == InvoiceEntryMode::Voucher && amount != 0 {
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
        let source_rates = xml_invoice_tax_rates(entry)?;
        if !source_rates.is_empty() {
            if amount == 0
                || is_party
                || has_stock
                || !bills.is_empty()
                || !matches!(category, Category::Asset | Category::Liability)
                || (amount > 0) == party_debit
            {
                return Err(Error::Validation(format!(
                    "Invoice tax on {ledger} needs a tax ledger and an amount in the invoice tax direction"
                )));
            }
            if !tax_ledgers.insert(normalized(&ledger)) {
                return Err(Error::Validation(format!(
                    "Invoice has more than one tax posting on {ledger}"
                )));
            }
            taxes.push(ImportedInvoiceTax {
                ledger,
                existing_ledger_id: *existing_ledger_id,
                amount: amount
                    .checked_abs()
                    .ok_or_else(|| Error::Validation("Invoice tax amount is too large".into()))?,
                source_rates,
            });
            continue;
        }
        if is_party {
            if !matches!(category, Category::Asset | Category::Liability) {
                return Err(Error::Validation(
                    "party ledger must be an asset or liability".into(),
                ));
            }
            if party.is_some()
                || (amount == 0 && !bills.is_empty())
                || (amount != 0 && ((amount > 0) != party_debit || bills.is_empty()))
            {
                return Err(Error::Validation(
                    "The document needs one party and bill allocations with the correct direction"
                        .into(),
                ));
            }
            let mut schedule = Vec::with_capacity(bills.len());
            let mut funding_applications = Vec::new();
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
                let terms = xml_bill_terms(credit.as_deref())?;
                match normalized(&invoice_field(bill, "BILLTYPE")?).as_str() {
                    "agst ref" => {
                        if !advance_references.insert(normalized(&reference)) {
                            return Err(Error::Validation(format!(
                                "Advance reference {reference} occurs more than once"
                            )));
                        }
                        funding_applications.push(ImportedFundingApplication {
                            reference,
                            amount,
                            terms,
                            expected_source: None,
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
                terms.due_date_from(parse_date(&date)?)?;
                schedule.push(BillSplitDraft {
                    reference,
                    amount,
                    due_date: terms.due_date,
                    credit_days: terms.credit_days,
                });
            }
            if signed_total != i128::from(amount) {
                return Err(Error::Validation(
                    "Bill allocations must add up to the full party amount".into(),
                ));
            }
            party = Some((ledger, amount, schedule, funding_applications));
        } else {
            if !bills.is_empty() {
                return Err(Error::Validation(
                    "additional ledger bill allocations are not supported".into(),
                ));
            }
            if !has_stock && amount == 0 && stock_item_ledger(&ledger, ledgers, kind).is_ok() {
                zero_trade_ledgers.insert(ledger.clone());
            }
            if !has_stock && amount != 0 {
                adjustments.push(ImportedInvoiceAdjustment {
                    ledger,
                    existing_ledger_id: *existing_ledger_id,
                    amount: if party_debit { -amount } else { amount },
                });
            }
        }
    }
    let order = orders::invoice_orders(node, &inventory_rows)?;
    let (mut lines, subtotal) = parse_invoice_stock(
        inventory_rows,
        breakdowns,
        kind,
        false,
        label,
        ledgers,
        items,
        locations,
        batches,
        issues,
    )?;
    i64::try_from(subtotal).map_err(|_| Error::Validation("invoice amount is too large".into()))?;
    let total = subtotal
        + adjustments
            .iter()
            .map(|row| i128::from(row.amount))
            .sum::<i128>()
        + taxes.iter().map(|tax| i128::from(tax.amount)).sum::<i128>();
    let stock_only = total == 0
        && lines.iter().any(|line| line.quantity > 0)
        && adjustments.is_empty()
        && taxes.is_empty();
    let (party_name, party_amount, bills, funding_applications) = match party {
        Some(party) => party,
        None if stock_only => {
            let party = header_party
                .as_ref()
                .ok_or_else(|| Error::Validation("party ledger is missing".into()))?;
            if !ledgers
                .get(&normalized(party))
                .is_some_and(|entry| matches!(entry.1, Category::Asset | Category::Liability))
            {
                return Err(Error::Validation(
                    "party ledger must be an available asset or liability".into(),
                ));
            }
            (party.clone(), 0, Vec::new(), Vec::new())
        }
        None => return Err(Error::Validation("party ledger is missing".into())),
    };
    if header_party.is_some_and(|name| normalized(&name) != normalized(&party_name)) {
        return Err(Error::Validation(
            "Voucher party differs from its bill ledger".into(),
        ));
    }
    if (total <= 0 && !stock_only) || i128::from(party_amount).abs() != total {
        return Err(Error::Validation(
            "party amount does not equal items, taxes and ledger adjustments".into(),
        ));
    }
    let trade = lines
        .iter()
        .find(|line| !line.trade_ledger.is_empty())
        .map(|line| line.trade_ledger.clone());
    let trade = match trade {
        Some(trade) => trade,
        None if stock_only => {
            let mut choices = zero_trade_ledgers.into_iter();
            match (choices.next(), choices.next()) {
                (Some(ledger), None) => ledger,
                _ => {
                    return Err(Error::Validation(
                        "A stock-only invoice needs an identifiable trade ledger".into(),
                    ))
                }
            }
        }
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
    if tax_ledgers.contains(&normalized(&trade)) {
        return Err(Error::Validation(
            "Invoice tax ledger must differ from the trade ledger".into(),
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
    for line in lines.iter_mut().filter(|line| line.trade_ledger.is_empty()) {
        line.trade_ledger = trade.clone();
        line.existing_trade_ledger_id = existing_trade_id;
    }
    Ok(ImportedInvoice {
        order,
        kind,
        entry_mode,
        source_order,
        date,
        reference,
        external_reference: invoice_document_reference(
            node,
            &INVOICE_REFERENCE_FIELDS,
            &INVOICE_REFERENCE_DATE_FIELDS,
        )?,
        bills,
        funding_applications,
        funded_bill_reference: None,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
        party: party_name,
        existing_party_id,
        trade,
        existing_trade_id,
        lines,
        adjustments,
        taxes,
        original_postings,
        metadata,
    })
}
struct StockLedgerBreakdown {
    ledger: String,
    posting_amount: i64,
    stock_amount: i128,
}

fn xml_item_amount(node: Node<'_, '_>) -> Result<i64> {
    direct_text(node, "AMOUNT")
        .filter(|value| !value.is_empty())
        .map_or(Ok(0), |value| xml_amount(&value))
}

fn xml_item_billed(
    node: Node<'_, '_>,
    unit: &UnitMeasure,
    actual: i64,
    amount: i64,
) -> Result<i64> {
    direct_text(node, "BILLEDQTY")
        .filter(|value| !value.is_empty())
        .map_or(Ok(if amount == 0 { 0 } else { actual }), |value| {
            unit.quantity(&value)
        })
}

fn stock_item_ledger(
    ledger: &str,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    kind: InvoiceKind,
) -> Result<Option<i64>> {
    let (id, category) = ledgers
        .get(&normalized(ledger))
        .ok_or_else(|| Error::Validation(format!("ledger {ledger} is unavailable")))?;
    if !trade_defaults::compatible(kind, *category) {
        return Err(Error::Validation(
            "item accounting ledger has an incompatible category".into(),
        ));
    }
    Ok(*id)
}

fn parse_invoice_stock(
    inventory_rows: Vec<(Node<'_, '_>, Option<usize>)>,
    mut breakdowns: Vec<StockLedgerBreakdown>,
    kind: InvoiceKind,
    returning: bool,
    label: &str,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    items: &InvoiceStockItems,
    locations: &HashMap<String, Option<i64>>,
    batches: &HashMap<(String, String), i64>,
    issues: &mut Vec<String>,
) -> Result<(Vec<ImportedInvoiceLine>, i128)> {
    let party_debit = (kind == InvoiceKind::Sale) != returning;
    let mut lines = Vec::new();
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
                "TRACKINGNUMBER",
                "GODOWNNAME",
                "BASICORDERREF",
                "ORDERNO",
                "ORDERDUEDATE",
                "ACCOUNTINGALLOCATIONS.LIST",
                "BATCHALLOCATIONS.LIST",
            ],
            label,
            issues,
        );
        if returning && orders::has_line_order_fields(item_node) {
            return Err(Error::Validation(
                "Imported returns use the order on their original invoice".into(),
            ));
        }
        let name = invoice_field(item_node, "STOCKITEMNAME")?;
        let (existing_item_id, unit) = items
            .get(&normalized(&name))
            .ok_or_else(|| Error::Validation(format!("stock item {name} is unavailable")))?;
        let actual = unit.quantity(&invoice_field(item_node, "ACTUALQTY")?)?;
        let source_amount = xml_item_amount(item_node)?;
        let billed = xml_item_billed(item_node, unit, actual, source_amount)?;
        let direction = if actual == 0 {
            billed.signum()
        } else {
            actual.signum()
        };
        if direction == 0
            || (returning && actual == 0)
            || (billed != 0 && billed.signum() != direction)
            || (direction < 0 && kind == InvoiceKind::Purchase && !returning)
        {
            return Err(Error::Validation(
                "actual and billed quantities must follow the supported stock direction".into(),
            ));
        }
        let quantity = actual
            .checked_abs()
            .ok_or_else(|| Error::Validation("quantity is too large".into()))?;
        let billed_quantity = billed
            .checked_abs()
            .ok_or_else(|| Error::Validation("billed quantity is too large".into()))?;
        let step = 10_i64.pow(u32::from(3 - unit.precision));
        if quantity % step != 0 || billed_quantity % step != 0 {
            return Err(Error::Validation("quantity exceeds unit precision".into()));
        }
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
        let (unit_price, rate_basis) =
            if let Some(rate) = direct_text(item_node, "RATE").filter(|v| !v.is_empty()) {
                let (price, rate_unit) = rate
                    .split_once('/')
                    .ok_or_else(|| Error::Validation("rate must specify the base unit".into()))?;
                let basis = unit.rate_basis(rate_unit)?;
                (parse_amount(price)?, basis)
            } else if source_amount == 0 {
                (0, None)
            } else {
                if posting.is_none() || !matches!(discount, InvoiceDiscount::None) {
                    return Err(Error::Validation("item rate is missing".into()));
                }
                let numerator = i128::from(source_amount).abs() * 1000;
                let quantity = i128::from(billed_quantity);
                if quantity == 0 || numerator % quantity != 0 {
                    return Err(Error::Validation(
                        "An omitted item rate cannot be derived exactly in hundredths".into(),
                    ));
                }
                (
                    i64::try_from(numerator / quantity)
                        .map_err(|_| Error::Validation("item rate is too large".into()))?,
                    None,
                )
            };
        let (amount, _) = discounted_line_amount_with_basis(
            billed_quantity,
            unit_price,
            &discount,
            rate_basis.as_ref(),
        )?;
        let expected = if party_debit { -amount } else { amount };
        if source_amount != expected {
            return Err(Error::Validation(
                "item amount differs from quantity and rate".into(),
            ));
        }
        if expected != 0 {
            invoice_side(item_node, "ISDEEMEDPOSITIVE", !party_debit)?;
        }
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
        } else if expected == 0
            && allocations
                .iter()
                .all(|allocation| !meaningful(*allocation))
        {
            String::new()
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
            if xml_item_amount(allocation)? != expected {
                return Err(Error::Validation(
                    "item accounting allocation differs from the item amount".into(),
                ));
            }
            if expected != 0 {
                invoice_side(allocation, "ISDEEMEDPOSITIVE", !party_debit)?;
            }
            invoice_field(allocation, "LEDGERNAME")?
        };
        let existing_trade_ledger_id = if ledger.is_empty() {
            None
        } else {
            stock_item_ledger(&ledger, ledgers, kind)?
        };
        let locations_in_line: Vec<_> = item_node
            .children()
            .filter(|n| {
                n.is_element()
                    && local_name(*n).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
                    && meaningful(*n)
            })
            .collect();
        let mut stock_allocations = Vec::new();
        let item_tracking = direct_text(item_node, "TRACKINGNUMBER");
        let item_location = direct_text(item_node, "GODOWNNAME").filter(|name| !name.is_empty());
        if locations_in_line.is_empty() {
            let location = item_location.unwrap_or_else(|| "Main".into());
            let existing_location_id = *locations
                .get(&normalized(&location))
                .ok_or_else(|| Error::Validation("default stock location is unavailable".into()))?;
            stock_allocations.push(ImportedStockAllocation {
                location,
                existing_location_id,
                batch: None,
                existing_batch_id: None,
                quantity,
                billed_quantity,
                tracking: stock_notes::tracking(item_tracking),
            });
        } else {
            if locations_in_line.len() > 1 && matches!(discount, InvoiceDiscount::Fixed(_)) {
                return Err(Error::Validation(
                    "fixed discounts across multiple stock allocations are not supported".into(),
                ));
            }
            let mut total_quantity = 0_i128;
            let mut total_amount = 0_i128;
            let mut total_billed = 0_i128;
            for location_node in locations_in_line {
                invoice_list_fields(
                    location_node,
                    &[
                        "GODOWNNAME",
                        "DESTINATIONGODOWNNAME",
                        "BATCHNAME",
                        "ACTUALQTY",
                        "BILLEDQTY",
                        "AMOUNT",
                        "TRACKINGNUMBER",
                        "BASICORDERREF",
                        "ORDERNO",
                        "ORDERDUEDATE",
                    ],
                    label,
                    issues,
                );
                if returning && orders::has_line_order_fields(location_node) {
                    return Err(Error::Validation(
                        "Imported returns use the order on their original invoice".into(),
                    ));
                }
                let allocated_actual =
                    unit.quantity(&invoice_field(location_node, "ACTUALQTY")?)?;
                let source_allocation_amount = xml_item_amount(location_node)?;
                let allocated_billed = xml_item_billed(
                    location_node,
                    unit,
                    allocated_actual,
                    source_allocation_amount,
                )?;
                if (allocated_actual == 0 && (allocated_billed == 0 || returning))
                    || (allocated_actual != 0 && allocated_actual.signum() != direction)
                    || (allocated_billed != 0 && allocated_billed.signum() != direction)
                {
                    return Err(Error::Validation(
                        "location quantities do not match the item direction".into(),
                    ));
                }
                let allocated_quantity = allocated_actual
                    .checked_abs()
                    .ok_or_else(|| Error::Validation("quantity is too large".into()))?;
                let allocated_billed = allocated_billed
                    .checked_abs()
                    .ok_or_else(|| Error::Validation("billed quantity is too large".into()))?;
                if allocated_quantity % step != 0 || allocated_billed % step != 0 {
                    return Err(Error::Validation(
                        "allocation exceeds unit precision".into(),
                    ));
                }
                let (allocated_amount, _) = discounted_line_amount_with_basis(
                    allocated_billed,
                    unit_price,
                    &discount,
                    rate_basis.as_ref(),
                )?;
                let expected_amount = if party_debit {
                    -allocated_amount
                } else {
                    allocated_amount
                };
                if source_allocation_amount != expected_amount {
                    return Err(Error::Validation(
                        "allocation amount differs from quantity and rate".into(),
                    ));
                }
                let location = direct_text(location_node, "GODOWNNAME")
                    .filter(|name| !name.is_empty())
                    .or_else(|| item_location.clone())
                    .ok_or_else(|| Error::Validation("Missing GODOWNNAME".into()))?;
                if direct_text(location_node, "DESTINATIONGODOWNNAME").is_some_and(|destination| {
                    !destination.is_empty() && normalized(&destination) != normalized(&location)
                }) {
                    return Err(Error::Validation(
                        "Invoice batch source and destination locations differ".into(),
                    ));
                }
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
                total_billed += i128::from(allocated_billed);
                stock_allocations.push(ImportedStockAllocation {
                    location,
                    existing_location_id,
                    batch,
                    existing_batch_id,
                    quantity: allocated_quantity,
                    billed_quantity: allocated_billed,
                    tracking: stock_notes::tracking(
                        direct_text(location_node, "TRACKINGNUMBER")
                            .or_else(|| item_tracking.clone()),
                    ),
                });
            }
            if total_quantity != i128::from(quantity)
                || total_billed != i128::from(billed_quantity)
                || total_amount != i128::from(amount)
            {
                return Err(Error::Validation(
                    "location allocations do not match the item totals".into(),
                ));
            }
        }
        if !returning
            && stock_allocations
                .iter()
                .any(|allocation| allocation.tracking.is_some() && allocation.billed_quantity == 0)
        {
            return Err(Error::Validation(
                "A tracked invoice allocation needs a positive billed quantity".into(),
            ));
        }
        subtotal += i128::from(amount);
        lines.push(ImportedInvoiceLine {
            item: name,
            existing_item_id: *existing_item_id,
            unit: unit.symbol.clone(),
            precision: unit.precision,
            allocations: stock_allocations,
            existing_trade_ledger_id,
            trade_ledger: ledger,
            quantity,
            billed_quantity,
            unit_price,
            rate_basis,
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
    Ok((lines, subtotal))
}

type ReferenceKey = (String, String, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FundingSource {
    Advance(Option<i64>),
    Note(Option<i64>),
}
impl FundingSource {
    fn label(self) -> &'static str {
        match self {
            Self::Advance(_) => "advance",
            Self::Note(_) => "credit",
        }
    }

    fn matches(self, current: Self) -> bool {
        matches!(
            (self, current),
            (Self::Advance(None), Self::Advance(Some(_))) | (Self::Note(None), Self::Note(Some(_)))
        ) || self == current
    }
}

struct NamedFunding {
    source: FundingSource,
    reference: String,
    date: NaiveDate,
    original_date: NaiveDate,
    due_date: Option<NaiveDate>,
    latest_release: Option<NaiveDate>,
    available: i128,
}

enum FundingMatch {
    Unique(NamedFunding),
    Ambiguous { includes_credit: bool },
}
impl FundingMatch {
    fn includes_credit(&self) -> bool {
        match self {
            Self::Unique(entry) => matches!(entry.source, FundingSource::Note(_)),
            Self::Ambiguous { includes_credit } => *includes_credit,
        }
    }
}

type NamedFundingIndex = HashMap<ReferenceKey, FundingMatch>;

fn funding_key(kind: InvoiceKind, party: &str, reference: &str) -> ReferenceKey {
    (
        normalized(kind.as_str()),
        normalized(party),
        normalized(reference),
    )
}

fn index_stock_origin(
    origins: &mut HashMap<ReferenceKey, Vec<StockInvoiceOrigin>>,
    key: ReferenceKey,
    origin: StockInvoiceOrigin,
) {
    if let Some(number) = &origin.external_reference.number {
        let number_key = normalized(number);
        if number_key != key.2 {
            origins
                .entry((key.0.clone(), key.1.clone(), number_key))
                .or_default()
                .push(origin.clone());
        }
    }
    origins.entry(key).or_default().push(origin);
}

fn add_named_funding(
    advances: &mut NamedFundingIndex,
    kind: InvoiceKind,
    party: &str,
    funding: NamedFunding,
) -> bool {
    let key = funding_key(kind, party, &funding.reference);
    match advances.entry(key) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(FundingMatch::Unique(funding));
            true
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            let includes_credit =
                entry.get().includes_credit() || matches!(funding.source, FundingSource::Note(_));
            entry.insert(FundingMatch::Ambiguous { includes_credit });
            false
        }
    }
}

fn read_named_funding(conn: &Connection, company: i64) -> Result<NamedFundingIndex> {
    let mut advances = HashMap::new();
    for row in read_party_advances(conn, company, None)? {
        let date = parse_date(&row.date)?;
        let original_date = row
            .opening_original_date
            .as_deref()
            .map(parse_date)
            .transpose()?
            .unwrap_or(date);
        let (reference, due_date) = if let Some(source) = row.source_bill {
            let Some(reference) = source.named_reference().map(str::to_owned) else {
                continue;
            };
            let terms = BillTerms {
                due_date: source.due_date,
                credit_days: source.credit_days,
            };
            (reference, terms.due_date_from(original_date)?)
        } else {
            (row.reference, None)
        };
        add_named_funding(
            &mut advances,
            row.kind,
            &row.party,
            NamedFunding {
                source: FundingSource::Advance(Some(row.id)),
                reference,
                date,
                original_date,
                due_date,
                latest_release: advance_allocation_state(conn, company, row.id, None)?
                    .latest_release,
                available: row.available,
            },
        );
    }
    for credit in crate::bill_note_credits::read_credits(conn, company, None, None)? {
        let date = parse_date(&credit.date)?;
        let terms = BillTerms {
            due_date: credit.due_date,
            credit_days: credit.credit_days,
        };
        add_named_funding(
            &mut advances,
            credit.kind,
            &credit.party,
            NamedFunding {
                source: FundingSource::Note(Some(credit.id)),
                reference: credit.reference,
                date,
                original_date: date,
                due_date: terms.due_date_from(date)?,
                latest_release: credit.latest_release,
                available: credit.available,
            },
        );
    }
    Ok(advances)
}

fn resolve_named_funding<'a>(
    advances: &'a mut NamedFundingIndex,
    kind: InvoiceKind,
    party: &str,
    date: NaiveDate,
    application: &ImportedFundingApplication,
) -> Result<&'a mut NamedFunding> {
    let reference = &application.reference;
    let key = funding_key(kind, party, reference);
    let advance = match advances.get_mut(&key) {
        Some(FundingMatch::Unique(entry)) => entry,
        Some(FundingMatch::Ambiguous { .. }) => {
            return Err(Error::Validation(format!(
                "Named advance or credit {reference} is ambiguous"
            )))
        }
        None => {
            return Err(Error::Validation(format!(
                "Named advance or credit {reference} is unavailable for this party and type"
            )))
        }
    };
    let label = advance.source.label();
    if &advance.reference != reference {
        return Err(Error::Validation(format!(
            "Reference {reference} differs from the saved {label} spelling"
        )));
    }
    if date < advance.date || advance.latest_release.is_some_and(|release| date < release) {
        return Err(Error::Validation(format!(
            "Application precedes {label} {reference} or its latest release"
        )));
    }
    if !application
        .terms
        .matches_due_date(advance.original_date, advance.due_date)?
    {
        return Err(Error::Validation(format!(
            "Credit terms differ from the saved {label} {reference}"
        )));
    }
    if i128::from(application.amount) > advance.available {
        return Err(Error::Validation(format!(
            "Application exceeds the available amount on {label} {reference}"
        )));
    }
    Ok(advance)
}

fn consume_previewed_funding(
    funding: &mut NamedFundingIndex,
    kind: InvoiceKind,
    party: &str,
    date: NaiveDate,
    application: &ImportedFundingApplication,
) -> Result<FundingSource> {
    let entry = resolve_named_funding(funding, kind, party, date, application)?;
    if !application
        .expected_source
        .is_some_and(|expected| expected.matches(entry.source))
        || entry.available != application.expected_available
    {
        return Err(Error::Validation(format!(
            "Named {} {} changed since preview",
            entry.source.label(),
            application.reference
        )));
    }
    entry.available -= i128::from(application.amount);
    Ok(entry.source)
}

fn preview_funding_applications(
    conn: &Connection,
    plan: &mut ImportPlan,
    bill_references: &mut HashSet<ReferenceKey>,
) -> Result<NamedFundingIndex> {
    let mut advances = read_named_funding(conn, plan.company_id)?;
    if let Some(opening) = plan.opening_date.as_deref() {
        let date = parse_date(opening)?;
        for account in &plan.accounts {
            for bill in account
                .opening_references
                .iter()
                .filter(|bill| bill.advance_type.is_some())
            {
                let original_date = parse_date(&bill.original_date)?;
                add_named_funding(
                    &mut advances,
                    bill.kind,
                    &account.name,
                    NamedFunding {
                        source: FundingSource::Advance(None),
                        reference: bill.reference.clone(),
                        date,
                        original_date,
                        due_date: bill.terms.due_date_from(original_date)?,
                        latest_release: None,
                        available: i128::from(bill.amount),
                    },
                );
            }
        }
    }
    for settlement in &plan.settlements {
        for ImportedAdvance {
            amount,
            source_bill,
        } in &settlement.bills.advances
        {
            if let Some(reference) = source_bill.named_reference() {
                let date = parse_date(&settlement.date)?;
                let terms = BillTerms {
                    due_date: source_bill.due_date.clone(),
                    credit_days: source_bill.credit_days,
                };
                add_named_funding(
                    &mut advances,
                    settlement.kind,
                    &settlement.party,
                    NamedFunding {
                        source: FundingSource::Advance(None),
                        reference: reference.into(),
                        date,
                        original_date: date,
                        due_date: terms.due_date_from(date)?,
                        latest_release: None,
                        available: (*amount).into(),
                    },
                );
            }
        }
    }
    for note in &plan.bill_notes {
        let date = parse_date(&note.date)?;
        for credit in &note.credits {
            if !add_named_funding(
                &mut advances,
                note.kind,
                &note.lines[note.party_index].0.ledger,
                NamedFunding {
                    source: FundingSource::Note(None),
                    reference: credit.reference.clone(),
                    date,
                    original_date: date,
                    due_date: credit.terms.due_date_from(date)?,
                    latest_release: None,
                    available: i128::from(credit.amount),
                },
            ) {
                plan.issues.push(format!(
                    "Voucher {}: named credit {} is ambiguous for this party and type",
                    note.reference, credit.reference
                ));
            }
        }
    }
    for invoice in plan
        .invoices
        .iter_mut()
        .filter(|invoice| !invoice.funding_applications.is_empty())
    {
        let date = parse_date(&invoice.date)?;
        let mut funded = 0_i64;
        for application in &mut invoice.funding_applications {
            funded = funded
                .checked_add(application.amount)
                .ok_or_else(|| Error::Validation("Invoice advance total is too large".into()))?;
            match resolve_named_funding(
                &mut advances,
                invoice.kind,
                &invoice.party,
                date,
                application,
            ) {
                Ok(advance) => {
                    application.expected_source = Some(advance.source);
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
        let label =
            if invoice.funding_applications.iter().any(|application| {
                matches!(application.expected_source, Some(FundingSource::Note(_)))
            }) {
                "credit applied"
            } else {
                "advance"
            };
        let prefix = format!("{} ({label})", invoice.reference);
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
        invoice.funded_bill_reference = Some(reference);
    }
    Ok(advances)
}

fn preview_refund_allocations(
    settlement: &mut ImportedSettlement,
    funding: &mut NamedFundingIndex,
    bill_exists: impl Fn(&ReferenceKey) -> bool,
    issues: &mut Vec<String>,
) -> Result<()> {
    let note_kind = bill_note_refunds::opposite_kind(settlement.kind);
    let date = parse_date(&settlement.date)?;
    let label = format!("Voucher {}", settlement.reference);
    for allocation in std::mem::take(&mut settlement.bills.invoices) {
        let note_key = funding_key(note_kind, &settlement.party, &allocation.reference);
        let candidate = funding
            .get(&note_key)
            .is_some_and(FundingMatch::includes_credit);
        if !candidate {
            settlement.bills.invoices.push(allocation);
            continue;
        }
        if bill_exists(&funding_key(
            settlement.kind,
            &settlement.party,
            &allocation.reference,
        )) {
            issues.push(format!(
                "{label}: reference {} matches both a bill and a refund credit",
                allocation.reference
            ));
            continue;
        }
        let mut refund = ImportedFundingApplication {
            reference: allocation.reference,
            amount: allocation.amount,
            terms: allocation.terms,
            expected_source: None,
            expected_available: 0,
        };
        match resolve_named_funding(funding, note_kind, &settlement.party, date, &refund) {
            Ok(credit) => {
                refund.expected_source = Some(credit.source);
                refund.expected_available = credit.available;
                credit.available -= i128::from(refund.amount);
                settlement.refunds.push(refund);
            }
            Err(error) => issues.push(format!("{label}: {error}")),
        }
    }
    let discounts = settlement
        .discounts
        .iter()
        .map(|row| i128::from(row.amount))
        .sum::<i128>();
    let billed = settlement
        .bills
        .invoices
        .iter()
        .map(|row| i128::from(row.amount))
        .sum::<i128>();
    if discounts > billed {
        issues.push(format!(
            "{label}: discounts cannot exceed the amount allocated to bills"
        ));
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

fn import_ledger(
    conn: &Connection,
    company: i64,
    name: &str,
    existing: Option<i64>,
) -> Result<i64> {
    let id = find_ledger(conn, company, name)?
        .ok_or_else(|| Error::Validation(format!("Ledger {name} is missing")))?;
    if existing.is_some_and(|expected| expected != id) {
        return Err(Error::Validation(format!(
            "Ledger {name} changed since preview"
        )));
    }
    Ok(id)
}

fn resolve_imported_bill_allocations(
    conn: &Connection,
    company: i64,
    kind: InvoiceKind,
    party: i64,
    date: &str,
    source: &[ImportedBillAllocation],
) -> Result<Vec<BillSettlementAllocation>> {
    let date = parse_date(date)?;
    let mut stmt = conn.prepare_cached(
        "SELECT i.id, b.id, COALESCE(b.reference, i.bill_reference, v.reference),
                CASE WHEN b.id IS NULL THEN i.due_date ELSE b.due_date END,
                COALESCE(o.original_date, v.date)
         FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
         LEFT JOIN invoice_bill_splits b ON b.invoice_id = i.id
         LEFT JOIN opening_bills o ON o.invoice_id = i.id
         WHERE i.company_id = ?1 AND i.kind = ?2 AND i.party_ledger_id = ?3
           AND EXISTS(SELECT 1 FROM postings p WHERE p.voucher_id = v.id)",
    )?;
    let mut existing = HashMap::<String, Vec<_>>::new();
    for row in stmt.query_map(params![company, kind.as_str(), party], |r| {
        Ok((
            BillSettlementTarget {
                invoice_id: r.get(0)?,
                split_id: r.get(1)?,
            },
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
        ))
    })? {
        let (target, reference, due_date, original_date) = row?;
        existing.entry(normalized(&reference)).or_default().push((
            target,
            reference,
            due_date,
            original_date,
        ));
    }
    let mut allocations = Vec::with_capacity(source.len());
    for allocation in source {
        let changed = || {
            Error::Validation(format!(
                "Bill {} changed or became ambiguous since preview",
                allocation.reference
            ))
        };
        let candidates = existing
            .get(&normalized(&allocation.reference))
            .ok_or_else(|| {
                Error::Validation(format!("Bill {} is missing", allocation.reference))
            })?;
        let [(target, reference, due_date, original_date)] = candidates.as_slice() else {
            return Err(changed());
        };
        if reference != &allocation.reference
            || allocation
                .existing_target
                .is_some_and(|expected| expected != *target)
            || !allocation.terms.matches_due_date(
                parse_date(original_date)?,
                due_date.as_deref().map(parse_date).transpose()?,
            )?
        {
            return Err(changed());
        }
        let unpaid = crate::invoice_credits::unpaid_for_allocation(
            conn,
            company,
            target.invoice_id,
            kind,
            party,
            date,
        )?;
        let outstanding =
            crate::bill_splits::target(conn, *target)?.map_or(unpaid, |bill| bill.outstanding());
        if outstanding != allocation.expected_outstanding
            || i128::from(allocation.amount) > outstanding
        {
            return Err(changed());
        }
        allocations.push(BillSettlementAllocation {
            target: *target,
            amount: allocation.amount,
        });
    }
    Ok(allocations)
}

fn post_imported_bill_note(
    conn: &Connection,
    company: i64,
    source: &ImportedBillNote,
) -> Result<(i64, i64)> {
    let kind = source.voucher_kind();
    check_new_bill_voucher(conn, company, kind, &source.date, &source.reference)?;
    let mut postings = Vec::with_capacity(source.lines.len());
    let mut party_lines = Vec::new();
    for (position, (line, existing)) in source.lines.iter().enumerate() {
        let ledger_id = import_ledger(conn, company, &line.ledger, *existing)?;
        postings.push(Posting {
            ledger_id,
            amount: line.amount,
        });
        if line.is_party {
            party_lines.push(position);
        }
    }
    let party = postings[source.party_index].ledger_id;
    let voucher = insert_voucher_draft(
        conn,
        company,
        VoucherDraft {
            kind,
            date: &source.date,
            reference: &source.reference,
            narration: &source.narration,
            postings: &postings,
            party_lines: &party_lines,
        },
    )?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO bill_note_credits(voucher_id, party_ledger_id, reference, amount, due_date, credit_days)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for credit in &source.credits {
        stmt.execute(params![
            voucher,
            party,
            credit.reference,
            credit.amount,
            credit.terms.due_date,
            credit.terms.credit_days
        ])?;
    }
    save_metadata(conn, company, "voucher", voucher, &source.metadata)?;
    Ok((voucher, party))
}

fn attach_note_bill_allocations(
    conn: &Connection,
    company: i64,
    source: &ImportedBillNote,
    voucher: i64,
    party: i64,
) -> Result<()> {
    let allocations = resolve_imported_bill_allocations(
        conn,
        company,
        source.kind,
        party,
        &source.date,
        &source.bills,
    )?;
    write_note_bill_allocations(conn, voucher, &allocations)
}

fn write_note_bill_allocations(
    conn: &Connection,
    voucher: i64,
    allocations: &[BillSettlementAllocation],
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO bill_note_allocations(voucher_id, invoice_id, split_id, amount) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for allocation in allocations {
        stmt.execute(params![
            voucher,
            allocation.target.invoice_id,
            allocation.target.split_id,
            allocation.amount
        ])?;
    }
    Ok(())
}

fn resolve_return_invoice(
    conn: &Connection,
    company: i64,
    party: i64,
    source: &ImportedBillNote,
    bills: &[BillSettlementAllocation],
) -> Result<i64> {
    let invoice = if let Some(original) = &source.original_invoice {
        let mut stmt = conn.prepare_cached(
            "SELECT i.id, v.reference, v.date, i.external_reference, i.external_reference_date
             FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
             WHERE i.company_id = ?1 AND i.party_ledger_id = ?2 AND i.kind = ?3
               AND (EXISTS(SELECT 1 FROM invoice_lines l WHERE l.invoice_id = i.id AND l.quantity > 0)
                    OR EXISTS(SELECT 1 FROM active_invoice_stock_links s WHERE s.invoice_id = i.id))
               AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = i.id)",
        )?;
        let mut candidates = Vec::new();
        for row in stmt.query_map(params![company, party, source.kind.as_str()], |row| {
            Ok(StockInvoiceOrigin {
                existing_id: Some(row.get(0)?),
                voucher_number: row.get(1)?,
                voucher_date: row.get(2)?,
                external_reference: InvoiceExternalReference {
                    number: row.get(3)?,
                    date: row.get(4)?,
                },
            })
        })? {
            let candidate = row?;
            if let Some(exact) = candidate.matching_spelling(original) {
                candidates.push((candidate, exact));
            }
        }
        let [(candidate, exact)] = candidates.as_slice() else {
            return Err(Error::Validation(
                "Original invoice is missing or ambiguous".into(),
            ));
        };
        if !exact
            || original
                .existing_id
                .is_some_and(|expected| Some(expected) != candidate.existing_id)
        {
            return Err(Error::Validation(
                "Original invoice changed since preview".into(),
            ));
        }
        if parse_date(&candidate.voucher_date)? > parse_date(&source.date)? {
            return Err(Error::Validation(
                "Return cannot precede the original invoice".into(),
            ));
        }
        candidate.existing_id.expect("saved invoice has an ID")
    } else {
        bills
            .first()
            .ok_or_else(|| {
                Error::Validation("Stock return needs the original invoice reference".into())
            })?
            .target
            .invoice_id
    };
    if bills.iter().any(|row| row.target.invoice_id != invoice) {
        return Err(Error::Validation(
            "Stock return must apply to bills on the original invoice".into(),
        ));
    }
    if crate::invoice_reversals::for_invoice(conn, company, invoice)?.is_some() {
        return Err(Error::Validation(
            "A reversed invoice cannot accept a stock return".into(),
        ));
    }
    Ok(invoice)
}

fn apply_stock_return(
    conn: &Connection,
    company: i64,
    source: &ImportedBillNote,
    voucher: Option<i64>,
) -> Result<()> {
    let (party, existing) = &source.lines[source.party_index];
    let party_id = import_ledger(conn, company, &party.ledger, *existing)?;
    let bills = resolve_imported_bill_allocations(
        conn,
        company,
        source.kind,
        party_id,
        &source.date,
        &source.bills,
    )?;
    let invoice = resolve_return_invoice(conn, company, party_id, source, &bills)?;
    let allocations = bills
        .iter()
        .filter_map(|row| {
            row.target
                .split_id
                .map(|split_id| crate::invoice_credits::NoteBillAllocation {
                    split_id,
                    amount: row.amount,
                })
        })
        .collect::<Vec<_>>();
    let mut stock = Vec::new();
    for line in &source.stock {
        let item = find_stock_item(conn, company, &line.item)?
            .ok_or_else(|| Error::Validation(format!("Stock item {} is unavailable", line.item)))?;
        if line
            .existing_item_id
            .is_some_and(|expected| expected != item)
        {
            return Err(Error::Validation(format!(
                "Stock item {} changed since preview",
                line.item
            )));
        }
        let ledger = import_ledger(
            conn,
            company,
            &line.trade_ledger,
            line.existing_trade_ledger_id,
        )?;
        for source_stock in &line.allocations {
            let location =
                find_stock_location(conn, company, &source_stock.location)?.ok_or_else(|| {
                    Error::Validation(format!(
                        "Stock location {} is unavailable",
                        source_stock.location
                    ))
                })?;
            if source_stock
                .existing_location_id
                .is_some_and(|expected| expected != location)
            {
                return Err(Error::Validation(format!(
                    "Stock location {} changed since preview",
                    source_stock.location
                )));
            }
            let batch = source_stock
                .batch
                .as_deref()
                .map(|name| {
                    find_stock_batch(conn, company, item, name)?.ok_or_else(|| {
                        Error::Validation(format!("Stock batch {name} is unavailable"))
                    })
                })
                .transpose()?;
            if source_stock
                .existing_batch_id
                .is_some_and(|expected| Some(expected) != batch)
            {
                return Err(Error::Validation(
                    "Stock batch changed since preview".into(),
                ));
            }
            stock.push(crate::invoice_returns::ImportedReturnAllocation {
                item,
                location,
                batch,
                ledger,
                quantity: source_stock.quantity,
                amount: discounted_line_amount_with_basis(
                    source_stock.billed_quantity,
                    line.unit_price,
                    &line.discount,
                    line.rate_basis.as_ref(),
                )?
                .0,
            });
        }
    }
    let mut expected = BTreeMap::<i64, i128>::new();
    for (line, existing) in &source.lines {
        let ledger_id = import_ledger(conn, company, &line.ledger, *existing)?;
        *expected.entry(ledger_id).or_default() += i128::from(line.amount);
    }
    let mut item_amounts = BTreeMap::<i64, i128>::new();
    for part in &stock {
        *item_amounts.entry(part.ledger).or_default() += i128::from(part.amount);
    }
    let sign = if source.kind == InvoiceKind::Sale {
        1
    } else {
        -1
    };
    if item_amounts
        .iter()
        .any(|(ledger, items)| sign * expected.get(ledger).copied().unwrap_or_default() < *items)
    {
        return Err(Error::Validation(
            "Return ledger postings do not cover the returned item amounts".into(),
        ));
    }
    let components = crate::invoice_credits::credit_components(conn, company, invoice)?;
    let mut adjustments = Vec::new();
    for (&ledger, &amount) in &expected {
        if ledger == party_id {
            continue;
        }
        let items = item_amounts.get(&ledger).copied().unwrap_or_default();
        let remainder = sign * amount - items;
        if remainder == 0 {
            continue;
        }
        let Some(amount) = i64::try_from(remainder).ok().filter(|amount| *amount > 0) else {
            return Err(Error::Validation(
                "Return posting differs from the item credit".into(),
            ));
        };
        let mut matches = components.iter().filter(|component| {
            let used = if component.is_tax { 0 } else { items };
            component.ledger_id == ledger && component.remaining - used >= remainder
        });
        let component = matches.next().ok_or_else(|| {
            Error::Validation("Return posting is unavailable on the original invoice".into())
        })?;
        if matches.next().is_some() {
            return Err(Error::Validation(
                "Return posting matches multiple original invoice components".into(),
            ));
        }
        adjustments.push(crate::invoice_credits::CreditLineDraft {
            ledger_id: ledger,
            is_tax: component.is_tax,
            amount,
        });
    }
    let prepared = crate::invoice_returns::prepare_imported_return(conn, company, invoice, &stock)?;
    let voucher = match voucher {
        Some(voucher) => Some(voucher),
        None if !prepared.amounts_match_original => {
            Some(post_imported_bill_note(conn, company, source)?.0)
        }
        None => None,
    };
    if let Some(voucher) = voucher {
        crate::invoice_returns::attach_imported_stock_return(
            conn,
            company,
            invoice,
            voucher,
            parse_date(&source.date)?,
            &source.reference,
            prepared,
        )?;
        return write_note_bill_allocations(conn, voucher, &bills);
    }
    let reason = if source.narration.is_empty() {
        "Imported stock return"
    } else {
        &source.narration
    };
    check_new_bill_voucher(
        conn,
        company,
        source.voucher_kind(),
        &source.date,
        &source.reference,
    )?;
    let note = crate::invoice_returns::insert_imported_return(
        conn,
        company,
        invoice,
        parse_date(&source.date)?,
        &source.reference,
        reason,
        prepared,
        &allocations,
        &adjustments,
    )?;
    let voucher: i64 = conn.query_row(
        "SELECT voucher_id FROM invoice_credit_notes WHERE id = ?1",
        [note],
        |row| row.get(0),
    )?;
    let mut stmt =
        conn.prepare_cached("SELECT ledger_id, amount FROM postings WHERE voucher_id = ?1")?;
    let mut posted = BTreeMap::<i64, i128>::new();
    for row in stmt.query_map([voucher], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (ledger, amount) = row?;
        *posted.entry(ledger).or_default() += i128::from(amount);
    }
    if posted != expected {
        return Err(Error::Validation(
            "Return ledger postings differ from the original invoice components".into(),
        ));
    }
    save_metadata(conn, company, "voucher", voucher, &source.metadata)
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
        if unit_key(&name) == unit_key(symbol) {
            if found.is_some() {
                return Err(Error::Validation(format!("Unit {symbol} is ambiguous")));
            }
            found = Some((id, places));
        }
    }
    Ok(found)
}

fn resolve_imported_item(
    conn: &Connection,
    company: i64,
    name: &str,
    expected_id: Option<i64>,
    source_unit: &str,
    source_precision: u8,
) -> Result<i64> {
    let item_id = find_stock_item(conn, company, name)?
        .ok_or_else(|| Error::Validation(format!("Stock item {} is missing", name)))?;
    if expected_id.is_some_and(|previous| previous != item_id) {
        return Err(Error::Validation(format!(
            "Stock item {} changed since preview",
            name
        )));
    }
    let (unit, precision): (String, u8) = conn.query_row("SELECT u.symbol, u.decimal_places FROM stock_items i JOIN stock_units u ON u.id = i.unit_id AND u.company_id = i.company_id WHERE i.id = ?1 AND i.company_id = ?2", params![item_id, company], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if normalized(&unit) != normalized(source_unit) || precision != source_precision {
        return Err(Error::Validation(format!(
            "Unit for {} changed since preview",
            name
        )));
    }
    Ok(item_id)
}

fn resolve_imported_allocation(
    conn: &Connection,
    company: i64,
    item_id: i64,
    allocation: &ImportedStockAllocation,
    created_batches: &mut HashMap<(i64, String), i64>,
) -> Result<(i64, Option<i64>)> {
    let location_id =
        find_stock_location(conn, company, &allocation.location)?.ok_or_else(|| {
            Error::Validation(format!("Stock location {} is missing", allocation.location))
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
        let existing = find_stock_batch(conn, company, item_id, name)?;
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
            let id = create_stock_batch_on_connection(conn, company, item_id, name, None, None)?;
            created_batches.insert(key, id);
            id
        };
        Some(id)
    } else {
        None
    };
    Ok((location_id, batch_id))
}

type InvoiceStockItems = HashMap<String, (Option<i64>, Arc<UnitMeasure>)>;

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
        self.preview_xml_import_batch_with_stock_counts(company_id, files, opening_date, None)
    }

    /// Select whether imported physical counts adjust stock or record observations.
    pub fn preview_xml_import_batch_with_stock_counts(
        &self,
        company_id: i64,
        files: &[&[u8]],
        opening_date: Option<&str>,
        count_mode: Option<crate::stock_counts::StockCountMode>,
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
        // A voucher owns its contents. Nested elements cannot become independent
        // masters or postings, including inside an archived voucher.
        let records = documents
            .iter()
            .flat_map(Document::descendants)
            .filter(|node| {
                node.is_element()
                    && !node.ancestors().skip(1).any(|parent| {
                        parent.is_element() && local_name(parent).eq_ignore_ascii_case("VOUCHER")
                    })
            })
            .collect::<Vec<_>>();
        let nodes = || records.iter().copied();
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
            stock_notes: Vec::new(),
            stock_returns: Vec::new(),
            stock_counts: Vec::new(),
            stock_journals: Vec::new(),
            cancelled_vouchers: Vec::new(),
            optional_vouchers: Vec::new(),
            orders: Vec::new(),
            stock_preview: None,
            settlements: Vec::new(),
            bill_notes: Vec::new(),
            issues: Vec::new(),
        };
        let mut seen_metadata = HashSet::<(String, String, String)>::new();
        {
            let mut stmt = self.conn.prepare("SELECT kind, guid, remote_id, master_id, voucher_key FROM imported_record_metadata WHERE company_id = ?1")?;
            let rows = stmt.query_map([company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            for row in rows {
                let (kind, guid, remote, master, voucher_key) = row?;
                for (key, value) in [
                    ("guid", guid),
                    ("remote", remote),
                    ("master", master),
                    ("voucher key", voucher_key),
                ] {
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
            let supported = [
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
                || (local_name(node).eq_ignore_ascii_case("VOUCHERTYPE")
                    && stock_journals::type_name(node).is_some());
            if !supported && meaningful(node) {
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
            let net_opening = match direct_text(node, "OPENINGBALANCE").filter(|s| !s.is_empty()) {
                Some(value) => match xml_amount(&value) {
                    Ok(0) => Some(0),
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
            let opening_references = opening_references(
                node,
                net_opening,
                category,
                plan.opening_date.as_deref(),
                &label,
                &mut plan.issues,
            );
            let signed_bills: i128 = opening_references
                .iter()
                .map(|bill| {
                    let amount = i128::from(bill.amount);
                    match (bill.kind, bill.advance_type.is_some()) {
                        (InvoiceKind::Sale, false) | (InvoiceKind::Purchase, true) => amount,
                        _ => -amount,
                    }
                })
                .sum();
            let remainder = i128::from(net_opening.unwrap_or_default()) - signed_bills;
            let opening = match i64::try_from(remainder) {
                Ok(0) => None,
                Ok(amount) if amount != i64::MIN => Some(amount),
                _ => {
                    plan.issues.push(format!(
                        "{label}: on-account opening amount is outside the supported range"
                    ));
                    None
                }
            };
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
                opening_references,
                credit_days,
                metadata,
            });
        }
        let existing_units = self.stock_units(company_id)?;
        let mut known_units = saved_measures(&existing_units)?;
        let mut seen_units = HashSet::new();
        let mut compounds = Vec::new();
        for node in
            nodes().filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("UNIT"))
        {
            let kind = normalized(&direct_text(node, "ISSIMPLEUNIT").unwrap_or_default());
            let symbol = object_name(node).unwrap_or_default();
            let has_name = direct_text(node, "NAME").is_some_and(|name| !name.is_empty())
                || node.attributes().any(|attribute| {
                    attribute.name().eq_ignore_ascii_case("NAME")
                        && !attribute.value().trim().is_empty()
                });
            if symbol.is_empty() && (kind != "no" || has_name) {
                plan.issues.push("A unit has no unambiguous symbol".into());
                continue;
            }
            let label = if symbol.is_empty() {
                "Compound unit".into()
            } else {
                format!("Unit {symbol}")
            };
            let mut fields = vec![
                "NAME",
                "ORIGINALNAME",
                "ISSIMPLEUNIT",
                "DECIMALPLACES",
                "LANGUAGENAME.LIST",
            ];
            if kind == "no" {
                fields.extend(["BASEUNITS", "ADDITIONALUNITS", "CONVERSION"]);
            }
            check_fields(node, &fields, &label, &mut plan.issues);
            check_language_names(node, &symbol, &label, &mut plan.issues);
            if !symbol.is_empty() && !seen_units.insert(unit_key(&symbol)) {
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
            let formal_name = direct_text(node, "ORIGINALNAME").unwrap_or_default();
            if kind == "no" {
                let compound = (|| -> Result<CompoundUnit> {
                    Ok(CompoundUnit {
                        primary: invoice_field(node, "BASEUNITS")?,
                        tail: invoice_field(node, "ADDITIONALUNITS")?,
                        factor: stock_units::factor(&invoice_field(node, "CONVERSION")?)?,
                    })
                })();
                match compound {
                    Ok(compound) => compounds.push((symbol, formal_name, compound, metadata)),
                    Err(error) => plan.issues.push(format!("{label}: {error}")),
                }
                continue;
            }
            if kind != "yes" {
                plan.issues
                    .push(format!("{label}: expected a simple or compound unit flag"));
                continue;
            }
            let places = match direct_text(node, "DECIMALPLACES")
                .filter(|value| !value.is_empty())
                .as_deref()
                .unwrap_or("0")
                .parse::<u8>()
                .ok()
                .filter(|places| *places <= 3)
            {
                Some(places) => places,
                None => {
                    plan.issues
                        .push(format!("{label}: decimal places must be between 0 and 3"));
                    continue;
                }
            };
            if let Some(existing) = existing_units
                .iter()
                .find(|u| unit_key(&u.symbol) == unit_key(&symbol))
            {
                if existing.compound.is_some()
                    || existing.decimal_places != places
                    || (!formal_name.is_empty() && existing.formal_name != formal_name)
                {
                    plan.issues
                        .push(format!("{label}: existing unit has different details"));
                }
                if !metadata.is_empty() {
                    plan.existing_unit_metadata.push((existing.id, metadata));
                }
            } else {
                if let Err(error) =
                    index_measure(&mut known_units, UnitMeasure::simple(&symbol, places))
                {
                    plan.issues.push(error.to_string());
                    continue;
                }
                plan.units.push(Unit {
                    symbol,
                    formal_name,
                    decimal_places: places,
                    compound: None,
                    metadata,
                });
            }
        }
        while !compounds.is_empty() {
            let count = compounds.len();
            let mut unresolved = Vec::new();
            for (symbol, formal_name, mut compound, metadata) in compounds {
                let measure = match resolve_compound(&symbol, &compound, &known_units) {
                    Ok(Some(measure)) => measure,
                    Ok(None) => {
                        unresolved.push((symbol, formal_name, compound, metadata));
                        continue;
                    }
                    Err(error) => {
                        plan.issues.push(format!("Compound unit {symbol}: {error}"));
                        continue;
                    }
                };
                compound.primary = known_units[&unit_key(&compound.primary)].symbol.clone();
                compound.tail = known_units[&unit_key(&compound.tail)].symbol.clone();
                let key = unit_key(&measure.symbol);
                let existing_name = known_units
                    .get(&key)
                    .or_else(|| known_units.get(&unit_key(&measure.canonical)));
                if let Some(existing_name) = existing_name {
                    let existing = existing_units
                        .iter()
                        .find(|entry| entry.symbol == existing_name.symbol);
                    match existing {
                        Some(existing)
                            if existing.compound.as_ref() == Some(&compound)
                                && (formal_name.is_empty()
                                    || formal_name == existing.formal_name) =>
                        {
                            let existing_measure = Arc::clone(existing_name);
                            known_units.insert(key, existing_measure);
                            if !metadata.is_empty() {
                                plan.existing_unit_metadata.push((existing.id, metadata));
                            }
                        }
                        _ => plan.issues.push(format!(
                            "Unit {} is duplicated or has different components",
                            measure.symbol
                        )),
                    }
                    continue;
                }
                let symbol = measure.symbol.clone();
                let decimal_places = measure.precision;
                if let Err(error) = index_measure(&mut known_units, measure) {
                    plan.issues.push(error.to_string());
                    continue;
                }
                plan.units.push(Unit {
                    symbol,
                    formal_name,
                    decimal_places,
                    compound: Some(compound),
                    metadata,
                });
            }
            if unresolved.len() == count {
                for (symbol, _, compound, _) in unresolved {
                    plan.issues.push(format!(
                        "Compound unit {symbol}: components {} and {} are missing or cyclic",
                        compound.primary, compound.tail
                    ));
                }
                break;
            }
            compounds = unresolved;
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
            let unit_name = direct_text(node, "BASEUNITS").unwrap_or_default();
            let Some(unit) = known_units.get(&unit_key(&unit_name)) else {
                plan.issues
                    .push(format!("{label}: unit {unit_name} is not available"));
                continue;
            };
            let precision = unit.precision;
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
                Some(value) => match unit.quantity(&value) {
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
                unit,
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
                    match xml_stock_location(&requested_location, &known_locations) {
                        Some(location) => location,
                        None => {
                            plan.issues.push(format!(
                                "{label}: stock location {requested_location} is unavailable"
                            ));
                            continue;
                        }
                    };
                if let Some(expected) = &top_location {
                    if !xml_stock_location(expected, &known_locations)
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
                        Some(value) => match unit.quantity(&value) {
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
                    unit,
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
                        xml_stock_location(request, &known_locations).unwrap_or_else(|| {
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
                unit: unit.symbol.clone(),
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
        let mut invoice_items = InvoiceStockItems::new();
        let unit_symbols: HashMap<_, _> = existing_units
            .iter()
            .map(|unit| (unit.id, unit.symbol.as_str()))
            .collect();
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
            if let Some(symbol) = unit_symbols.get(&item.unit_id) {
                invoice_items.insert(
                    normalized(&item.name),
                    (Some(item.id), Arc::clone(&known_units[&unit_key(symbol)])),
                );
            }
        }
        invoice_items.extend(plan.items.iter().map(|item| {
            (
                normalized(&item.name),
                (None, Arc::clone(&known_units[&unit_key(&item.unit)])),
            )
        }));
        let mut invoice_references = HashSet::<(String, String, String)>::new();
        let mut bill_references = HashSet::<(String, String, String)>::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT i.kind, l.name, v.reference,
                        CASE WHEN EXISTS(SELECT 1 FROM postings p WHERE p.voucher_id = v.id)
                             THEN COALESCE(b.reference, i.bill_reference, v.reference) END
                 FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
                 LEFT JOIN invoice_bill_splits b ON b.invoice_id = i.id
                 JOIN ledgers l ON l.id = i.party_ledger_id WHERE i.company_id = ?1",
            )?;
            for row in stmt.query_map([company_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })? {
                let (kind, party, reference, bill_reference) = row?;
                let kind = normalized(&kind);
                let party = normalized(&party);
                invoice_references.insert((kind.clone(), party.clone(), normalized(&reference)));
                if let Some(bill_reference) = bill_reference {
                    bill_references.insert((kind, party, normalized(&bill_reference)));
                }
            }
        }
        let mut pending_settlements = Vec::new();
        for account in &plan.accounts {
            for bill in account
                .opening_references
                .iter()
                .filter(|bill| bill.advance_type.is_none())
            {
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
        let mut stock_note_references = HashSet::new();
        let mut order_references = HashSet::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT o.kind, l.name, o.reference_key FROM orders o
                 JOIN ledgers l ON l.id = o.party_ledger_id WHERE o.company_id = ?1",
            )?;
            let rows = stmt.query_map([company_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    normalized(&row.get::<_, String>(1)?),
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                order_references.insert(row?);
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT n.kind, l.name, n.reference_key FROM stock_notes n
                 JOIN ledgers l ON l.id = n.party_ledger_id WHERE n.company_id = ?1",
            )?;
            let rows = stmt.query_map([company_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    normalized(&row.get::<_, String>(1)?),
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                stock_note_references.insert(row?);
            }
        }
        let journal_types = nodes()
            .filter(|node| {
                node.is_element() && local_name(*node).eq_ignore_ascii_case("VOUCHERTYPE")
            })
            .filter_map(stock_journals::type_name)
            .map(|name| normalized(&name))
            .collect::<HashSet<_>>();
        let mut cancelled_headers = cancelled::existing_headers(&self.conn, company_id)?;
        let mut optional_payloads = HashSet::new();
        for (source_order, node) in nodes()
            .filter(|n| n.is_element() && local_name(*n).eq_ignore_ascii_case("VOUCHER"))
            .enumerate()
        {
            let label = format!(
                "Voucher {}",
                direct_text(node, "VOUCHERNUMBER").unwrap_or_default()
            );
            let metadata = source_metadata(node, &label, &mut plan.issues);
            match cancelled::status(node) {
                Ok(cancelled::Status::Cancelled) => {
                    match cancelled::parse(node, metadata) {
                        Ok(record) => {
                            register_metadata(
                                cancelled::KIND,
                                &label,
                                &record.metadata,
                                &mut seen_metadata,
                                &mut plan.issues,
                            );
                            if record
                                .header_without_id()
                                .is_some_and(|key| !cancelled_headers.insert(key))
                            {
                                plan.issues.push(format!(
                                    "{label}: this cancelled voucher already exists"
                                ));
                            }
                            plan.cancelled_vouchers.push(record);
                        }
                        Err(error) => plan.issues.push(format!("{label}: {error}")),
                    }
                    continue;
                }
                Ok(cancelled::Status::Optional) => {
                    match cancelled::ImportedOptional::parse(node, metadata) {
                        Ok(record) => {
                            register_metadata(
                                cancelled::OPTIONAL_KIND,
                                &label,
                                &record.header.metadata,
                                &mut seen_metadata,
                                &mut plan.issues,
                            );
                            if record.duplicate(&self.conn, company_id)?
                                || (record.unkeyed()
                                    && !optional_payloads.insert(record.payload.clone()))
                            {
                                plan.issues
                                    .push(format!("{label}: this optional voucher already exists"));
                            }
                            plan.optional_vouchers.push(record);
                        }
                        Err(error) => plan.issues.push(format!("{label}: {error}")),
                    }
                    continue;
                }
                Err(error) => {
                    plan.issues.push(format!("{label}: {error}"));
                    continue;
                }
                Ok(cancelled::Status::Posted) => {}
            }
            let voucher_type = direct_text(node, "VOUCHERTYPENAME");
            let stock_kind = voucher_type.as_deref().and_then(stock_notes::kind);
            let order_kind = voucher_type.as_deref().and_then(orders::kind);
            let rejection_kind = voucher_type.as_deref().and_then(stock_rejections::kind);
            let physical_stock = voucher_type.as_deref().is_some_and(stock_counts::kind);
            let journal_type = voucher_type
                .clone()
                .or_else(|| {
                    node.attributes()
                        .find(|attr| attr.name().eq_ignore_ascii_case("VCHTYPE"))
                        .map(|attr| attr.value().trim().to_owned())
                })
                .filter(|name| {
                    stock_journals::kind(name) || journal_types.contains(&normalized(name))
                });
            register_metadata(
                if stock_kind.is_some() {
                    "stocknote"
                } else if order_kind.is_some() {
                    "order"
                } else if rejection_kind.is_some() {
                    "stockreturn"
                } else if physical_stock {
                    "stockcount"
                } else if journal_type.is_some() {
                    "stockjournal"
                } else {
                    "voucher"
                },
                &label,
                &metadata,
                &mut seen_metadata,
                &mut plan.issues,
            );
            if let Some(kind) = order_kind {
                match orders::parse(
                    node,
                    kind,
                    &label,
                    metadata,
                    &invoice_ledgers,
                    &invoice_items,
                    &mut plan.issues,
                ) {
                    Ok(order) => {
                        if !order_references.insert((
                            kind.as_str().to_owned(),
                            normalized(&order.party),
                            normalized(&order.reference),
                        )) {
                            plan.issues.push(format!("{label}: an order with the same type, party and reference already exists"));
                        }
                        plan.orders.push(order);
                    }
                    Err(error) => plan.issues.push(format!("{label}: {error}")),
                }
                continue;
            }
            if let Some(kind) = stock_kind {
                match stock_notes::parse(
                    node,
                    kind,
                    source_order,
                    &label,
                    metadata,
                    &invoice_ledgers,
                    &invoice_items,
                    &known_locations,
                    &invoice_batches,
                    &mut plan.issues,
                ) {
                    Ok(note) => {
                        if !stock_note_references.insert((
                            kind.as_str().to_owned(),
                            normalized(&note.party),
                            normalized(&note.reference),
                        )) {
                            plan.issues.push(format!("{label}: a stock note with the same type, party and reference already exists"));
                        }
                        plan.stock_notes.push(note);
                    }
                    Err(error) => plan.issues.push(format!("{label}: {error}")),
                }
                continue;
            }
            if let Some(kind) = rejection_kind {
                match stock_rejections::parse(
                    node,
                    kind,
                    source_order,
                    &label,
                    metadata,
                    &invoice_ledgers,
                    &invoice_items,
                    &mut plan.issues,
                ) {
                    Ok(rejection) => plan.stock_returns.push(rejection),
                    Err(error) => plan.issues.push(format!("{label}: {error}")),
                }
                continue;
            }
            if let Some(journal_type) = journal_type {
                match stock_journals::parse(
                    node,
                    source_order,
                    journal_type,
                    &label,
                    metadata,
                    &invoice_items,
                    &known_locations,
                    &invoice_batches,
                    &mut plan.issues,
                ) {
                    Ok(journal) => plan.stock_journals.push(journal),
                    Err(error) => plan.issues.push(format!("{label}: {error}")),
                }
                continue;
            }
            if physical_stock {
                if let Some(mode) = count_mode {
                    match stock_counts::parse(
                        node,
                        source_order,
                        mode,
                        &label,
                        metadata,
                        &invoice_items,
                        &known_locations,
                        &invoice_batches,
                        &mut plan.issues,
                    ) {
                        Ok(count) => plan.stock_counts.push(count),
                        Err(error) => plan.issues.push(format!("{label}: {error}")),
                    }
                } else {
                    let message =
                        "Select how physical stock vouchers should affect stock before previewing.";
                    if !plan.issues.iter().any(|issue| issue == message) {
                        plan.issues.push(message.into());
                    }
                }
                continue;
            }
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
            let kind = voucher_type.as_deref().and_then(parse_kind);
            if has_bill_details
                && matches!(kind, Some(VoucherKind::CreditNote | VoucherKind::DebitNote))
            {
                match parse_imported_bill_note(
                    node,
                    &label,
                    source_order,
                    metadata,
                    &invoice_ledgers,
                    &invoice_items,
                    &known_locations,
                    &invoice_batches,
                    &mut plan.issues,
                ) {
                    Ok(note) => {
                        if !references.insert((
                            normalized(note.voucher_kind().as_str()),
                            note.date.clone(),
                            normalized(&note.reference),
                        )) {
                            plan.issues.push(format!("{label}: a voucher with the same type, date and number already exists"));
                        }
                        plan.bill_notes.push(note);
                    }
                    Err(error) => plan.issues.push(format!("{label}: {error}")),
                }
                continue;
            }
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
                    source_order,
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
                pending_settlements.push((node, label, source_order, metadata));
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
            for (node, label, source_order, metadata) in pending_settlements {
                match parse_imported_settlement(
                    node,
                    &label,
                    source_order,
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
                        if !settlement.bills.advances.is_empty()
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
        let has_funding_applications = plan
            .invoices
            .iter()
            .any(|invoice| !invoice.funding_applications.is_empty());
        let mut funding = if has_funding_applications
            || plan.note_credit_count() > 0
            || plan.settlement_count() > 0
        {
            preview_funding_applications(&self.conn, &mut plan, &mut bill_references)?
        } else {
            HashMap::new()
        };
        let bill_changes = plan.bill_changes();
        if !bill_changes.is_empty() {
            struct PreviewBill {
                target: Option<BillSettlementTarget>,
                invoice_key: ReferenceKey,
                invoice_date: NaiveDate,
                original_date: NaiveDate,
                due_date: Option<NaiveDate>,
                reference: String,
                outstanding: i128,
                latest_reversal: Option<NaiveDate>,
            }
            let mut invoice_balances = HashMap::<ReferenceKey, PreviewBill>::new();
            let mut payment_references = HashMap::<ReferenceKey, HashSet<String>>::new();
            let mut stock_origins = HashMap::<ReferenceKey, Vec<StockInvoiceOrigin>>::new();
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
                let has_matched_stock = invoice.lines.iter().any(|line| {
                    line.allocations.iter().any(|allocation| {
                        allocation.linked_stock_quantity > 0
                            || (!plan.stock_notes.is_empty()
                                && allocation.source_tracking.is_some())
                    })
                });
                if invoice.reversal.is_none()
                    && (has_matched_stock || invoice.lines.iter().any(|line| line.quantity > 0))
                {
                    index_stock_origin(
                        &mut stock_origins,
                        invoice_key.clone(),
                        StockInvoiceOrigin {
                            voucher_number: invoice.reference.clone(),
                            voucher_date: invoice.date.clone(),
                            external_reference: invoice.external_reference,
                            existing_id: Some(invoice.id),
                        },
                    );
                }
                if invoice.total == 0 {
                    continue;
                }
                let used = payment_references.entry(invoice_key.clone()).or_default();
                for settlement in self.invoice_settlements(company_id, invoice.id)? {
                    used.insert(normalized(&settlement.reference));
                    if let Some(date) = settlement.reversal_date {
                        let date = parse_date(&date)?;
                        latest_reversal =
                            Some(latest_reversal.map_or(date, |old: NaiveDate| old.max(date)));
                    }
                }
                let invoice_date = parse_date(&invoice.date)?;
                let original_date = invoice
                    .opening_bill
                    .as_ref()
                    .map(|bill| parse_date(&bill.original_date))
                    .transpose()?
                    .unwrap_or(invoice_date);
                let entries = if invoice.bill_splits.is_empty() {
                    vec![(
                        None,
                        invoice.bill_reference,
                        invoice.outstanding,
                        invoice.due_date,
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
                            (Some(bill.id), bill.reference, remaining, bill.due_date)
                        })
                        .collect()
                };
                for (split_id, reference, outstanding, due_date) in entries {
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
                                invoice_date,
                                original_date,
                                due_date: due_date.as_deref().map(parse_date).transpose()?,
                                reference,
                                outstanding,
                                latest_reversal,
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
                let invoice_date = parse_date(&invoice.date)?;
                let invoice_key = (
                    normalized(invoice.kind.as_str()),
                    normalized(&invoice.party),
                    normalized(&invoice.reference),
                );
                if invoice.lines.iter().any(|line| {
                    line.quantity > 0
                        || line
                            .allocations
                            .iter()
                            .any(|allocation| allocation.tracking.is_some())
                }) {
                    index_stock_origin(
                        &mut stock_origins,
                        invoice_key.clone(),
                        StockInvoiceOrigin {
                            voucher_number: invoice.reference.clone(),
                            voucher_date: invoice.date.clone(),
                            external_reference: invoice.external_reference.clone(),
                            existing_id: None,
                        },
                    );
                }
                for bill in &invoice.bills {
                    if invoice.funded_bill_reference.as_ref() == Some(&bill.reference) {
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
                                invoice_date,
                                original_date: invoice_date,
                                due_date: bill_due_date(
                                    invoice_date,
                                    bill.due_date.as_deref(),
                                    bill.credit_days,
                                )?,
                                reference: bill.reference.clone(),
                                outstanding: i128::from(bill.amount),
                                latest_reversal: None,
                            },
                        )
                        .is_some()
                    {
                        plan.issues
                            .push("Imported bill references are ambiguous".into());
                    }
                }
            }
            if let Some(opening_date) = plan.opening_date.as_deref() {
                let opening_date = parse_date(opening_date)?;
                for account in &plan.accounts {
                    for bill in account
                        .opening_references
                        .iter()
                        .filter(|bill| bill.advance_type.is_none())
                    {
                        let original_date = parse_date(&bill.original_date)?;
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
                                    invoice_date: opening_date,
                                    original_date,
                                    due_date: bill.terms.due_date_from(original_date)?,
                                    reference: bill.reference.clone(),
                                    outstanding: i128::from(bill.amount),
                                    latest_reversal: None,
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
            for change in bill_changes {
                let (
                    kind,
                    party,
                    date,
                    reference,
                    allocations,
                    is_settlement,
                    is_stock_return,
                    origin_key,
                ) = match change {
                    BillChange::Settlement(index) => {
                        let row = &mut plan.settlements[index];
                        preview_refund_allocations(
                            row,
                            &mut funding,
                            |key| invoice_balances.contains_key(key),
                            &mut plan.issues,
                        )?;
                        (
                            row.kind,
                            &row.party,
                            &row.date,
                            &row.reference,
                            &mut row.bills.invoices,
                            true,
                            false,
                            None,
                        )
                    }
                    BillChange::Note(index) => {
                        let row = &mut plan.bill_notes[index];
                        let party = &row.lines[row.party_index].0.ledger;
                        let origin_key = if let Some(original) = &mut row.original_invoice {
                            let key = funding_key(row.kind, party, &original.reference);
                            let mut candidates =
                                stock_origins.get(&key).into_iter().flatten().filter_map(
                                    |candidate| {
                                        candidate
                                            .matching_spelling(original)
                                            .map(|exact| (candidate, exact))
                                    },
                                );
                            match (candidates.next(), candidates.next()) {
                                (Some((candidate, true)), None) => {
                                    if candidate.voucher_date > row.date {
                                        plan.issues.push(format!(
                                            "Voucher {}: return precedes its original invoice",
                                            row.reference
                                        ));
                                    }
                                    original.existing_id = candidate.existing_id;
                                    Some(funding_key(row.kind, party, &candidate.voucher_number))
                                }
                                _ => {
                                    plan.issues.push(format!("Voucher {}: original stock invoice {} is unavailable or ambiguous", row.reference, original.reference));
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        (
                            row.kind,
                            party,
                            &row.date,
                            &row.reference,
                            &mut row.bills,
                            false,
                            !row.stock.is_empty(),
                            origin_key,
                        )
                    }
                };
                let label = format!("Voucher {reference}");
                let date = parse_date(date)?;
                let mut referenced_invoices = HashSet::new();
                for allocation in allocations {
                    let key = (
                        normalized(kind.as_str()),
                        normalized(party),
                        normalized(&allocation.reference),
                    );
                    let Some(bill) = invoice_balances.get_mut(&key) else {
                        plan.issues.push(format!(
                            "{label}: referenced bill {} is unavailable",
                            allocation.reference
                        ));
                        continue;
                    };
                    if !allocation
                        .terms
                        .matches_due_date(bill.original_date, bill.due_date)?
                    {
                        plan.issues.push(format!(
                            "{label}: credit terms differ from the saved bill {}",
                            bill.reference
                        ));
                    }
                    if is_settlement
                        && referenced_invoices.insert(bill.invoice_key.clone())
                        && !payment_references
                            .entry(bill.invoice_key.clone())
                            .or_default()
                            .insert(normalized(reference))
                    {
                        plan.issues.push(format!(
                            "{label}: settlement reference already exists for the invoice"
                        ));
                    }
                    if is_stock_return
                        && referenced_invoices.insert(bill.invoice_key.clone())
                        && referenced_invoices.len() > 1
                    {
                        plan.issues.push(format!(
                            "{label}: stock return must reference one original invoice"
                        ));
                    }
                    if origin_key
                        .as_ref()
                        .is_some_and(|key| key != &bill.invoice_key)
                    {
                        plan.issues.push(format!("{label}: bill {} belongs to a different invoice than the returned stock", allocation.reference));
                    }
                    if bill.reference != allocation.reference {
                        plan.issues.push(format!(
                            "{label}: bill reference spelling differs from the saved bill"
                        ));
                    }
                    if date < bill.invoice_date
                        || bill.latest_reversal.is_some_and(|value| date < value)
                    {
                        plan.issues.push(format!("{label}: allocation precedes the invoice, a reversal or an advance release"));
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
                && (local_name(*n).eq_ignore_ascii_case("STOCKCATEGORY")
                    || (local_name(*n).eq_ignore_ascii_case("VOUCHERTYPE")
                        && stock_journals::type_name(*n).is_none()))
        }) {
            plan.issues
                .push(format!("{} records are not supported", local_name(node)));
        }
        if plan.groups.is_empty()
            && plan.accounts.is_empty()
            && plan.transactions.is_empty()
            && plan.invoices.is_empty()
            && plan.stock_notes.is_empty()
            && plan.stock_returns.is_empty()
            && plan.stock_counts.is_empty()
            && plan.stock_journals.is_empty()
            && plan.cancelled_vouchers.is_empty()
            && plan.optional_vouchers.is_empty()
            && plan.orders.is_empty()
            && plan.settlements.is_empty()
            && plan.bill_notes.is_empty()
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
        if plan.can_apply()
            && (!plan.stock_notes.is_empty()
                || !plan.stock_returns.is_empty()
                || !plan.stock_counts.is_empty()
                || !plan.stock_journals.is_empty()
                || !plan.orders.is_empty()
                || plan.invoices.iter().any(|invoice| invoice.order.is_some())
                || plan
                    .invoices
                    .iter()
                    .flat_map(|invoice| &invoice.lines)
                    .flat_map(|line| &line.allocations)
                    .any(|allocation| allocation.tracking.is_some()))
        {
            // Exercise the same stock and ledger checks without persisting the preview.
            let tx = self.conn.unchecked_transaction()?;
            let rehearsal = Self::write_xml_import(&tx, &plan);
            tx.rollback()?;
            match rehearsal {
                Ok((_, preview)) => plan.stock_preview = Some(preview),
                Err(error) => plan
                    .issues
                    .push(format!("Stock import validation failed. {error}")),
            }
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
        let (counts, actual) = Self::write_xml_import(&tx, plan)?;
        if let Some(preview) = &plan.stock_preview {
            if preview.matches != actual.matches {
                return Err(Error::Validation(
                    "Stock tracking matches changed since preview. Run the preview again.".into(),
                ));
            }
            if preview.returns != actual.returns {
                return Err(Error::Validation(
                    "Stock return allocations changed since preview. Run the preview again.".into(),
                ));
            }
            if preview.counts != actual.counts {
                return Err(Error::Validation("Stock count book quantities or allocations changed since preview. Run the preview again.".into()));
            }
        }
        tx.commit()?;
        Ok(counts)
    }

    fn write_xml_import(
        conn: &Connection,
        plan: &ImportPlan,
    ) -> Result<(ImportCounts, StockImportPreview)> {
        plan.check_period_lock(&conn)?;
        for record in &plan.cancelled_vouchers {
            cancelled::write(conn, plan.company_id, record)?;
        }
        for record in &plan.optional_vouchers {
            record.write(conn, plan.company_id)?;
        }
        for (id, metadata) in &plan.existing_location_metadata {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_locations WHERE id = ?1 AND company_id = ?2)",
                params![id, plan.company_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "A stock location changed since preview".into(),
                ));
            }
            save_metadata(&conn, plan.company_id, "stocklocation", *id, metadata)?;
        }
        for location in &plan.locations {
            if find_stock_location(&conn, plan.company_id, &location.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Stock location {} already exists",
                    location.name
                )));
            }
            let parent_id = match location.parent.as_deref() {
                Some(parent) => Some(
                    find_stock_location(&conn, plan.company_id, parent)?.ok_or_else(|| {
                        Error::Validation(format!(
                            "Stock location {}: parent {parent} is unavailable",
                            location.name
                        ))
                    })?,
                ),
                None => None,
            };
            conn.execute(
                "INSERT INTO stock_locations(company_id, name, parent_id) VALUES (?1, ?2, ?3)",
                params![plan.company_id, location.name, parent_id],
            )?;
            save_metadata(
                &conn,
                plan.company_id,
                "stocklocation",
                conn.last_insert_rowid(),
                &location.metadata,
            )?;
        }
        for (id, metadata) in &plan.existing_unit_metadata {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_units WHERE company_id = ?1 AND id = ?2)",
                params![plan.company_id, id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation("A unit changed since preview".into()));
            }
            save_metadata(&conn, plan.company_id, "stockunit", *id, metadata)?;
        }
        for (id, metadata) in &plan.existing_stock_group_metadata {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_groups WHERE company_id = ?1 AND id = ?2)",
                params![plan.company_id, id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "A stock group changed since preview".into(),
                ));
            }
            save_metadata(&conn, plan.company_id, "stockgroup", *id, metadata)?;
        }
        for unit in &plan.units {
            if find_stock_unit(&conn, plan.company_id, &unit.symbol)?.is_some() {
                return Err(Error::Validation(format!(
                    "Unit {} already exists",
                    unit.symbol
                )));
            }
            let id = if let Some(compound) = &unit.compound {
                let resolve = |symbol: &str| -> Result<i64> {
                    find_stock_unit(&conn, plan.company_id, symbol)?
                        .map(|(id, _)| id)
                        .ok_or_else(|| {
                            Error::Validation(format!(
                                "Compound unit component {symbol} is missing"
                            ))
                        })
                };
                stock_units::insert_compound(
                    &conn,
                    plan.company_id,
                    &unit.symbol,
                    &unit.formal_name,
                    resolve(&compound.primary)?,
                    resolve(&compound.tail)?,
                    compound.factor,
                )?
            } else {
                conn.execute("INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places) VALUES (?1, ?2, ?3, ?4)", params![plan.company_id, unit.symbol, unit.formal_name, unit.decimal_places])?;
                conn.last_insert_rowid()
            };
            save_metadata(&conn, plan.company_id, "stockunit", id, &unit.metadata)?;
        }
        for group in &plan.stock_groups {
            if find_stock_group(&conn, plan.company_id, &group.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Stock group {} already exists",
                    group.name
                )));
            }
            let parent =
                find_stock_group(&conn, plan.company_id, &group.parent)?.ok_or_else(|| {
                    Error::Validation(format!("Stock group {} is missing", group.parent))
                })?;
            conn.execute(
                "INSERT INTO stock_groups(company_id, name, parent_id) VALUES (?1, ?2, ?3)",
                params![plan.company_id, group.name, parent],
            )?;
            save_metadata(
                &conn,
                plan.company_id,
                "stockgroup",
                conn.last_insert_rowid(),
                &group.metadata,
            )?;
        }
        for (id, metadata) in &plan.existing_group_metadata {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM account_groups WHERE company_id = ?1 AND id = ?2)",
                params![plan.company_id, id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "An account group changed since preview".into(),
                ));
            }
            save_metadata(&conn, plan.company_id, "group", *id, metadata)?;
        }
        for group in &plan.groups {
            if find_group(&conn, plan.company_id, &group.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Group {} already exists",
                    group.name
                )));
            }
            let (parent, parent_category) = find_group(&conn, plan.company_id, &group.parent)?
                .ok_or_else(|| Error::Validation(format!("Group {} is missing", group.parent)))?;
            if parent_category != group.category {
                return Err(Error::Validation(format!(
                    "Group {} changed since preview",
                    group.parent
                )));
            }
            conn.execute("INSERT INTO account_groups(company_id, name, category, parent_id) VALUES (?1, ?2, ?3, ?4)", params![plan.company_id, group.name, group.category.as_str(), parent])?;
            save_metadata(
                &conn,
                plan.company_id,
                "group",
                conn.last_insert_rowid(),
                &group.metadata,
            )?;
        }
        for account in &plan.accounts {
            if crate::opening::is_control_name(&account.name) {
                return Err(Error::Validation(
                    "A ledger name is reserved for opening balances".into(),
                ));
            }
            if find_ledger(&conn, plan.company_id, &account.name)?.is_some() {
                return Err(Error::Validation(format!(
                    "Ledger {} already exists",
                    account.name
                )));
            }
            let (group_id, category) = find_group(&conn, plan.company_id, &account.group)?
                .ok_or_else(|| Error::Validation(format!("Group {} is missing", account.group)))?;
            if category != account.category {
                return Err(Error::Validation(format!(
                    "Group {} changed since preview",
                    account.group
                )));
            }
            conn.execute(
                "INSERT INTO ledgers(company_id, name, category, group_id) VALUES (?1, ?2, ?3, ?4)",
                params![plan.company_id, account.name, category.as_str(), group_id],
            )?;
            let ledger_id = conn.last_insert_rowid();
            save_metadata(
                &conn,
                plan.company_id,
                "ledger",
                ledger_id,
                &account.metadata,
            )?;
            insert_new_profile(&conn, plan.company_id, ledger_id, &account.profile)?;
            if let Some(days) = account.credit_days {
                conn.execute(
                    "INSERT INTO ledger_terms(company_id, ledger_id, credit_days) VALUES (?1, ?2, ?3)",
                    params![plan.company_id, ledger_id, days],
                )?;
            }
            if !account.opening_references.is_empty() {
                let date = plan
                    .opening_date
                    .as_deref()
                    .ok_or_else(|| Error::Validation("Opening date is required".into()))?;
                for bill in &account.opening_references {
                    if let Some(kind) = bill.advance_type {
                        let control = crate::opening::ensure_control(&conn, plan.company_id)?;
                        crate::advances::insert_opening_party_advance(
                            &conn,
                            plan.company_id,
                            &AdvanceDraft {
                                kind: bill.kind,
                                party_ledger_id: ledger_id,
                                bank_ledger_id: control,
                                date: date.into(),
                                reference: bill.reference.clone(),
                                amount: bill.amount,
                                narration: "Opening advance".into(),
                            },
                            &AdvanceSourceBill {
                                kind: kind.into(),
                                reference: Some(bill.reference.clone()),
                                due_date: bill.terms.due_date.clone(),
                                credit_days: bill.terms.credit_days,
                            },
                            parse_date(&bill.original_date)?,
                        )?;
                        continue;
                    }
                    crate::opening_bills::insert_opening_bill(
                        &conn,
                        plan.company_id,
                        &OpeningBillDraft {
                            kind: bill.kind,
                            party_ledger_id: ledger_id,
                            reference: bill.reference.clone(),
                            original_date: bill.original_date.clone(),
                            opening_date: date.to_owned(),
                            due_date: bill.terms.due_date.clone(),
                            credit_days: bill.terms.credit_days,
                            amount: bill.amount,
                        },
                    )?;
                }
            }
            if let Some(amount) = account.opening {
                let date = plan
                    .opening_date
                    .as_deref()
                    .ok_or_else(|| Error::Validation("Opening date is required".into()))?;
                crate::opening::save_opening_in_transaction(
                    &conn,
                    plan.company_id,
                    ledger_id,
                    date,
                    amount,
                    !account.opening_references.is_empty(),
                )?;
            }
        }
        let mut created_batches = HashMap::<(i64, String), i64>::new();
        for item in &plan.items {
            let (unit_id, precision) = find_stock_unit(&conn, plan.company_id, &item.unit)?
                .ok_or_else(|| Error::Validation(format!("Unit {} is missing", item.unit)))?;
            if precision != item.unit_precision {
                return Err(Error::Validation(format!(
                    "Unit {} changed since preview",
                    item.unit
                )));
            }
            let group_id =
                find_stock_group(&conn, plan.company_id, &item.group)?.ok_or_else(|| {
                    Error::Validation(format!("Stock group {} is missing", item.group))
                })?;
            let item_id = create_stock_item_on_connection(
                &conn,
                plan.company_id,
                &item.name,
                unit_id,
                group_id,
                &item.description,
            )?;
            save_metadata(&conn, plan.company_id, "stockitem", item_id, &item.metadata)?;
            for opening in &item.openings {
                let location = find_stock_location(&conn, plan.company_id, &opening.location)?
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
                            &conn,
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
                    check_quantity_precision(&conn, plan.company_id, item_id, opening.quantity)?;
                    let date = plan
                        .opening_date
                        .as_deref()
                        .ok_or_else(|| Error::Validation("Opening date is required".into()))?;
                    let adjustment = insert_stock_adjustment(
                        &conn,
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
                            &conn,
                            plan.company_id,
                            item_id,
                            location,
                            batch,
                        )?;
                    }
                    if let Some(cost) = opening.cost {
                        crate::actual_cost::insert_receipt_cost(
                            &conn,
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
                conn.prepare("SELECT kind, date, reference FROM vouchers WHERE company_id = ?1")?;
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
                    find_ledger(&conn, plan.company_id, &line.ledger)?.ok_or_else(|| {
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
                &conn,
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
            save_metadata(&conn, plan.company_id, "voucher", id, &voucher.metadata)?;
        }
        let settlement_ledgers = |settlement: &ImportedSettlement| -> Result<(i64, i64)> {
            let kind = if settlement.kind == InvoiceKind::Sale {
                VoucherKind::Receipt
            } else {
                VoucherKind::Payment
            };
            check_new_bill_voucher(
                &conn,
                plan.company_id,
                kind,
                &settlement.date,
                &settlement.reference,
            )?;
            let bank_id = import_ledger(
                &conn,
                plan.company_id,
                &settlement.bank,
                settlement.existing_bank_id,
            )?;
            let party_id = import_ledger(
                &conn,
                plan.company_id,
                &settlement.party,
                settlement.existing_party_id,
            )?;
            Ok((bank_id, party_id))
        };
        let settlement_discounts =
            |settlement: &ImportedSettlement| -> Result<Vec<SettlementDiscount>> {
                settlement
                    .discounts
                    .iter()
                    .map(|discount| {
                        let ledger_id = find_ledger(&conn, plan.company_id, &discount.ledger)?
                            .ok_or_else(|| {
                                Error::Validation(format!("Ledger {} is missing", discount.ledger))
                            })?;
                        if discount
                            .existing_ledger_id
                            .is_some_and(|expected| expected != ledger_id)
                        {
                            return Err(Error::Validation(format!(
                                "Ledger {} changed since preview",
                                discount.ledger
                            )));
                        }
                        Ok(SettlementDiscount {
                            ledger_id,
                            amount: discount.amount,
                        })
                    })
                    .collect()
            };
        let mut advance_vouchers = HashMap::new();
        for (index, settlement) in plan.settlements.iter().enumerate() {
            if settlement.bills.advances.is_empty() {
                continue;
            }
            let (bank_id, party_id) = settlement_ledgers(settlement)?;
            let parts = settlement
                .bills
                .advances
                .iter()
                .map(|advance| (advance.amount, Some(&advance.source_bill)))
                .collect::<Vec<_>>();
            let amount = i64::try_from(
                parts
                    .iter()
                    .map(|(amount, _)| i128::from(*amount))
                    .sum::<i128>(),
            )
            .map_err(|_| Error::Validation("Advance total is too large".into()))?;
            let (_, voucher_id) = insert_party_advance_parts(
                &conn,
                plan.company_id,
                &AdvanceDraft {
                    kind: settlement.kind,
                    party_ledger_id: party_id,
                    bank_ledger_id: bank_id,
                    date: settlement.date.clone(),
                    reference: settlement.reference.clone(),
                    amount,
                    narration: settlement.narration.clone(),
                },
                &parts,
                settlement
                    .bills
                    .invoices
                    .iter()
                    .try_fold(0_i64, |total, allocation| {
                        total.checked_add(allocation.amount).ok_or_else(|| {
                            Error::Validation("Settlement total is too large".into())
                        })
                    })?,
                &settlement_discounts(settlement)?,
                settlement.refund_total()?,
            )?;
            advance_vouchers.insert(index, (bank_id, party_id, voucher_id));
            save_metadata(
                &conn,
                plan.company_id,
                "voucher",
                voucher_id,
                &settlement.metadata,
            )?;
        }
        let mut posted_notes = vec![None; plan.bill_notes.len()];
        for (index, note) in plan.bill_notes.iter().enumerate() {
            if !note.credits.is_empty() {
                posted_notes[index] = Some(post_imported_bill_note(&conn, plan.company_id, note)?);
            }
        }
        let mut named_funding = if plan.note_credit_count() > 0
            || plan.note_refund_count() > 0
            || plan
                .invoices
                .iter()
                .any(|invoice| !invoice.funding_applications.is_empty())
        {
            read_named_funding(&conn, plan.company_id)?
        } else {
            HashMap::new()
        };
        for note in &plan.bill_notes {
            for credit in &note.credits {
                let key = funding_key(
                    note.kind,
                    &note.lines[note.party_index].0.ledger,
                    &credit.reference,
                );
                if !matches!(named_funding.get(&key), Some(FundingMatch::Unique(_))) {
                    return Err(Error::Validation(format!(
                        "Named credit {} is ambiguous for this party and type",
                        credit.reference
                    )));
                }
            }
        }
        for order in &plan.orders {
            orders::write(conn, plan.company_id, order)?;
        }
        let mut bill_changes = plan.bill_changes().into_iter().peekable();
        let mut write_bill_changes = |before: Option<(&str, usize)>,
                                      named_funding: &mut NamedFundingIndex|
         -> Result<()> {
            while let Some(change) =
                bill_changes.next_if(|change| before.is_none_or(|cutoff| change.key(plan) < cutoff))
            {
                let index = match change {
                    BillChange::Settlement(index) => index,
                    BillChange::Note(index) => {
                        let note = &plan.bill_notes[index];
                        if !note.stock.is_empty() {
                            apply_stock_return(
                                &conn,
                                plan.company_id,
                                note,
                                posted_notes[index].map(|(voucher, _)| voucher),
                            )?;
                            continue;
                        }
                        let (voucher, party) = match posted_notes[index] {
                            Some(saved) => saved,
                            None => post_imported_bill_note(&conn, plan.company_id, note)?,
                        };
                        attach_note_bill_allocations(&conn, plan.company_id, note, voucher, party)?;
                        continue;
                    }
                };
                let settlement = &plan.settlements[index];
                let (bank_id, party_id, advance_voucher) = match advance_vouchers.get(&index) {
                    Some(&(bank, party, voucher)) => (bank, party, Some(voucher)),
                    None => {
                        let (bank, party) = settlement_ledgers(settlement)?;
                        (bank, party, None)
                    }
                };
                let credit_kind = bill_note_refunds::opposite_kind(settlement.kind);
                let date = parse_date(&settlement.date)?;
                let mut refunds = Vec::with_capacity(settlement.refunds.len());
                for refund in &settlement.refunds {
                    let source = consume_previewed_funding(
                        named_funding,
                        credit_kind,
                        &settlement.party,
                        date,
                        refund,
                    )?;
                    let FundingSource::Note(Some(credit_id)) = source else {
                        return Err(Error::Validation("Refund credit was not saved".into()));
                    };
                    refunds.push(NoteRefundAllocation {
                        credit_id,
                        amount: refund.amount,
                    });
                }
                let refund_total = bill_note_refunds::validate_refunds(
                    &conn,
                    plan.company_id,
                    credit_kind,
                    party_id,
                    date,
                    &refunds,
                )?;
                let voucher_id = if !settlement.bills.invoices.is_empty() {
                    let allocations = resolve_imported_bill_allocations(
                        &conn,
                        plan.company_id,
                        settlement.kind,
                        party_id,
                        &settlement.date,
                        &settlement.bills.invoices,
                    )?;
                    insert_settlement_on_connection(
                        &conn,
                        plan.company_id,
                        bank_id,
                        &settlement.date,
                        &settlement.reference,
                        &allocations,
                        &settlement.narration,
                        advance_voucher,
                        &settlement_discounts(settlement)?,
                        refund_total,
                    )?
                    .0
                } else if let Some(voucher) = advance_voucher {
                    voucher
                } else {
                    bill_note_refunds::post_refund_voucher(
                        &conn,
                        plan.company_id,
                        settlement.kind,
                        bank_id,
                        party_id,
                        &settlement.date,
                        &settlement.reference,
                        &settlement.narration,
                        refund_total,
                    )?
                };
                bill_note_refunds::attach_refunds(&conn, voucher_id, bank_id, &refunds)?;
                if advance_voucher.is_none() {
                    save_metadata(
                        &conn,
                        plan.company_id,
                        "voucher",
                        voucher_id,
                        &settlement.metadata,
                    )?;
                }
            }
            Ok(())
        };
        let mut tracking = stock_notes::NewTracking::default();
        enum StockEvent<'a> {
            Note(&'a stock_notes::ImportedStockNote),
            Return(&'a stock_rejections::ImportedRejection),
            Count(&'a stock_counts::ImportedCount),
            Journal(&'a stock_journals::ImportedJournal),
        }
        impl<'a> StockEvent<'a> {
            fn key(&self, source_sequence: bool) -> (&'a str, bool, usize) {
                match self {
                    Self::Note(row) => (
                        &row.date,
                        !source_sequence && row.kind == InvoiceKind::Sale,
                        row.source_order,
                    ),
                    Self::Return(row) => (
                        &row.date,
                        !source_sequence && row.kind == InvoiceKind::Sale,
                        row.source_order,
                    ),
                    Self::Count(row) => (&row.date, false, row.source_order),
                    Self::Journal(row) => (&row.date, false, row.source_order),
                }
            }
        }
        let source_sequence = !plan.stock_counts.is_empty() || !plan.stock_journals.is_empty();
        let mut by_date = plan
            .stock_notes
            .iter()
            .map(StockEvent::Note)
            .chain(plan.stock_returns.iter().map(StockEvent::Return))
            .chain(plan.stock_counts.iter().map(StockEvent::Count))
            .chain(plan.stock_journals.iter().map(StockEvent::Journal))
            .collect::<Vec<_>>();
        by_date.sort_by_key(|event| event.key(source_sequence));
        let mut stock_events = by_date.into_iter().peekable();
        let mut stock_preview = StockImportPreview::default();
        let mut write_stock_events = |through: Option<(&str, bool, usize)>,
                                      tracking: &mut stock_notes::NewTracking,
                                      created_batches: &mut HashMap<(i64, String), i64>,
                                      named_funding: &mut NamedFundingIndex|
         -> Result<()> {
            while let Some(event) = stock_events.next_if(|event| {
                through.is_none_or(|through| event.key(source_sequence) <= through)
            }) {
                match event {
                    StockEvent::Note(note) => {
                        tracking.write_note(conn, plan.company_id, note, created_batches)?
                    }
                    StockEvent::Return(returned) => {
                        stock_preview.matches.extend(tracking.match_documents(
                            conn,
                            plan.company_id,
                            Some(stock_notes::TrackingCutoff {
                                date: &returned.date,
                                include_same_day_invoices: source_sequence,
                                references: Some(&returned.tracking_keys()),
                            }),
                        )?);
                        stock_preview.returns.extend(stock_rejections::write(
                            conn,
                            plan.company_id,
                            returned,
                        )?);
                    }
                    StockEvent::Journal(journal) => {
                        stock_journals::write(conn, plan.company_id, journal, created_batches)?;
                    }
                    StockEvent::Count(count) => {
                        stock_preview.matches.extend(tracking.match_documents(
                            conn,
                            plan.company_id,
                            Some(stock_notes::TrackingCutoff {
                                date: &count.date,
                                include_same_day_invoices: true,
                                references: None,
                            }),
                        )?);
                        write_bill_changes(Some((&count.date, count.source_order)), named_funding)?;
                        stock_preview.counts.extend(stock_counts::write(
                            conn,
                            plan.company_id,
                            count,
                            created_batches,
                        )?);
                    }
                }
            }
            Ok(())
        };
        let mut invoice_order = plan.invoices.iter().collect::<Vec<_>>();
        if source_sequence
            || !plan.stock_notes.is_empty()
            || !plan.stock_returns.is_empty()
            || plan.invoices.iter().any(|invoice| invoice.order.is_some())
        {
            invoice_order.sort_by_key(|row| {
                (
                    &row.date,
                    !source_sequence && row.kind == InvoiceKind::Sale,
                    row.source_order,
                )
            });
        }
        for invoice in invoice_order {
            write_stock_events(
                Some((
                    &invoice.date,
                    !source_sequence && invoice.kind == InvoiceKind::Sale,
                    if source_sequence {
                        invoice.source_order
                    } else {
                        usize::MAX
                    },
                )),
                &mut tracking,
                &mut created_batches,
                &mut named_funding,
            )?;
            let voucher_kind = if invoice.kind == InvoiceKind::Sale {
                VoucherKind::Sales
            } else {
                VoucherKind::Purchase
            };
            let mut stmt =
                conn.prepare("SELECT kind, date, reference FROM vouchers WHERE company_id = ?1")?;
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
                let id = find_ledger(&conn, plan.company_id, name)?
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
                let item_id = resolve_imported_item(
                    &conn,
                    plan.company_id,
                    &line.item,
                    line.existing_item_id,
                    &line.unit,
                    line.precision,
                )?;
                let mut allocations = Vec::with_capacity(line.allocations.len());
                for allocation in &line.allocations {
                    let (location_id, batch_id) = resolve_imported_allocation(
                        &conn,
                        plan.company_id,
                        item_id,
                        allocation,
                        &mut created_batches,
                    )?;
                    let quantity = if allocation.tracking.is_some() {
                        0
                    } else {
                        allocation.quantity
                    };
                    allocations.push(InvoiceAllocation {
                        location_id,
                        batch_id,
                        quantity,
                        billed_quantity: (allocation.billed_quantity != quantity)
                            .then_some(allocation.billed_quantity),
                    });
                }
                let physical_quantity = allocations
                    .iter()
                    .map(|allocation| allocation.quantity)
                    .sum();
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
                    quantity: physical_quantity,
                    billed_quantity: (line.billed_quantity != physical_quantity)
                        .then_some(line.billed_quantity),
                    unit_price: line.unit_price,
                    rate_basis: line.rate_basis.clone(),
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
                external_reference: invoice.external_reference.clone(),
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
                additional_taxes: invoice
                    .taxes
                    .iter()
                    .map(|tax| {
                        Ok(InvoiceTaxLine {
                            ledger_id: resolve_ledger(&tax.ledger, tax.existing_ledger_id)?,
                            amount: tax.amount,
                            source_rates: tax.source_rates.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
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
            let order_allocations = invoice
                .order
                .as_ref()
                .map(|orders| orders.resolve(conn, plan.company_id, &draft, invoice))
                .transpose()?;
            let (invoice_id, voucher_id) = insert_direct_invoice(
                &conn,
                plan.company_id,
                &draft,
                StockInvoicePosting::Import {
                    original_postings: (invoice.entry_mode == InvoiceEntryMode::Voucher)
                        .then_some(original_postings.as_slice()),
                    order_allocations: order_allocations.as_deref(),
                },
            )?;
            tracking.write_invoice(&conn, invoice_id, invoice)?;
            save_metadata(
                &conn,
                plan.company_id,
                "voucher",
                voucher_id,
                &invoice.metadata,
            )?;
            if let Some(reference) = &invoice.funded_bill_reference {
                let split_id = if draft.bill_splits.is_empty() {
                    None
                } else {
                    Some(
                        crate::bill_splits::read(&conn, invoice_id, None)?
                            .into_iter()
                            .find(|bill| &bill.reference == reference)
                            .ok_or_else(|| Error::Validation("Funded bill is missing".into()))?
                            .id,
                    )
                };
                let date = parse_date(&invoice.date)?;
                let target = BillSettlementTarget {
                    invoice_id,
                    split_id,
                };
                for application in &invoice.funding_applications {
                    let source = consume_previewed_funding(
                        &mut named_funding,
                        invoice.kind,
                        &invoice.party,
                        date,
                        application,
                    )?;
                    match source {
                        FundingSource::Advance(Some(advance_id)) => insert_advance_allocation(
                            &conn,
                            plan.company_id,
                            advance_id,
                            target,
                            date,
                            application.amount,
                            &invoice.narration,
                        )?,
                        FundingSource::Note(Some(credit_id)) => {
                            crate::bill_note_credits::insert_application(
                                &conn,
                                plan.company_id,
                                credit_id,
                                target,
                                date,
                                application.amount,
                            )?
                        }
                        _ => {
                            return Err(Error::Validation(
                                "Application source was not saved".into(),
                            ))
                        }
                    };
                }
            }
        }
        write_stock_events(
            None,
            &mut tracking,
            &mut created_batches,
            &mut named_funding,
        )?;
        stock_preview
            .matches
            .extend(tracking.match_documents(conn, plan.company_id, None)?);
        write_bill_changes(None, &mut named_funding)?;
        crate::stock_policy::check_import(&conn, plan.company_id)?;
        Ok((
            ImportCounts {
                groups: plan.groups.len(),
                ledgers: plan.accounts.len(),
                opening_bills: plan.opening_bill_count(),
                opening_advances: plan.opening_advance_count(),
                opening_on_account: plan.opening_on_account_count(),
                vouchers: plan.transactions.len(),
                invoices: plan.invoices.len(),
                stock_notes: plan.stock_notes.len(),
                stock_returns: plan.stock_returns.len(),
                orders: plan.orders.len(),
                stock_matches: stock_preview.matches.len(),
                stock_counts: plan.stock_counts.len(),
                stock_journals: plan.stock_journals.len(),
                cancelled_vouchers: plan.cancelled_vouchers.len(),
                optional_vouchers: plan.optional_vouchers.len(),
                settlements: plan.settlement_count(),
                bill_notes: plan.bill_note_count(),
                advances: plan.advance_count(),
                advance_applications: plan.advance_application_count(),
                note_credits: plan.note_credit_count(),
                credit_applications: plan.credit_application_count(),
                note_refunds: plan.note_refund_count(),
                units: plan.units.len(),
                stock_groups: plan.stock_groups.len(),
                locations: plan.locations.len(),
                items: plan.items.len(),
            },
            stock_preview,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_only_invoices_move_and_reverse_stock_without_financial_postings() {
        for (kind, sign, account, trade) in [
            ("Sales", -1_i64, Category::Asset, Category::Income),
            ("Purchase", 1_i64, Category::Liability, Category::Expense),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Stock-only invoices").unwrap();
            let party = store.create_ledger(company, "Party", account).unwrap();
            store.create_ledger(company, "Trade", trade).unwrap();
            let item = store.create_stock_item(company, "Sample", "pcs").unwrap();
            let initial = if sign < 0 { 2000 } else { 0 };
            if initial > 0 {
                let receipt = store
                    .adjust_stock(company, item, "2026-06-01", initial, "Opening")
                    .unwrap();
                store
                    .set_stock_receipt_cost(company, receipt, 1600, "Opening value")
                    .unwrap();
            }
            let zero_party = if sign < 0 {
                ""
            } else {
                "<LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>0</AMOUNT></LEDGERENTRIES.LIST>"
            };
            let xml = format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
                <VOUCHER><DATE>20260602</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME>
                <VOUCHERNUMBER>FREE-01</VOUCHERNUMBER><ISINVOICE>Yes</ISINVOICE>
                <PARTYLEDGERNAME>Party</PARTYLEDGERNAME>
                {zero_party}
                <LEDGERENTRIES.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>0</AMOUNT></LEDGERENTRIES.LIST>
                <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Sample</STOCKITEMNAME>
                    <ACTUALQTY>1 pcs</ACTUALQTY><BILLEDQTY>0 pcs</BILLEDQTY>
                </ALLINVENTORYENTRIES.LIST></VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#
            );
            let preview = store.preview_xml_import(company, xml.as_bytes()).unwrap();
            assert!(preview.can_apply(), "{:?}", preview.issues());
            assert_eq!(store.apply_xml_import(&preview).unwrap().invoices, 1);
            let invoice = store.invoices(company).unwrap().remove(0);
            assert_eq!((invoice.total, invoice.outstanding), (0, 0));
            assert_eq!(
                (invoice.lines[0].quantity, invoice.lines[0].billed_quantity),
                (1000, 0)
            );
            assert!(store.vouchers(company).unwrap()[0].postings.is_empty());
            assert_eq!(
                store.stock_balances(company, None).unwrap()[0].quantity,
                i128::from(initial + sign * 1000)
            );
            assert_eq!(
                store
                    .stock_valuation_at_average_cost(company, "2026-06-02")
                    .unwrap()
                    .known_value,
                if sign < 0 { 800 } else { 0 }
            );
            let mut draft = store.invoice_draft(company, invoice.id).unwrap();
            draft.narration = "Free stock".into();
            store.update_invoice(company, invoice.id, &draft).unwrap();
            assert_eq!(
                store.invoice_revisions(company, invoice.id).unwrap()[0].lines[0].billed_quantity,
                0
            );
            store
                .reverse_invoice(
                    company,
                    invoice.id,
                    "2026-06-03",
                    "FREE-REV",
                    "Test reversal",
                )
                .unwrap();
            assert_eq!(
                store.stock_balances(company, None).unwrap()[0].quantity,
                i128::from(initial)
            );
            let book = store.day_book(company, None, None, None, "FREE-").unwrap();
            assert_eq!(book.len(), 2);
            assert!(book
                .iter()
                .all(|entry| entry.debit_total == 0 && entry.credit_total == 0));
            assert_eq!(
                store
                    .ledger_register(company, party, None, None)
                    .unwrap()
                    .closing_balance,
                0
            );
            assert!(store
                .outstanding_invoices(company, "2026-06-03")
                .unwrap()
                .is_empty());
            draft.reference = "FREE-NATIVE".into();
            draft.date = "2026-06-04".into();
            draft.bill_reference = Some("NEW-BILL".into());
            assert!(store.create_invoice(company, &draft).is_err());
            draft.bill_reference = None;
            store.create_invoice(company, &draft).unwrap();
            assert_eq!(
                store.stock_balances(company, None).unwrap()[0].quantity,
                i128::from(initial + sign * 1000)
            );
            // A later monetary invoice can use the stock document's reference
            // as a bill reference without making settlement ambiguous.
            draft.reference = "MONETARY".into();
            draft.bill_reference = Some("FREE-01".into());
            draft.lines[0].billed_quantity = Some(1000);
            draft.lines[0].unit_price = 1000;
            store.create_invoice(company, &draft).unwrap();
            store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            let payment_kind = if sign < 0 { "Receipt" } else { "Payment" };
            let bank_amount = sign * 10;
            let party_amount = -bank_amount;
            let payment = format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
            <VOUCHER><DATE>20260605</DATE><VOUCHERTYPENAME>{payment_kind}</VOUCHERTYPENAME>
                <VOUCHERNUMBER>SETTLE</VOUCHERNUMBER><ISINVOICE>No</ISINVOICE>
                <PARTYLEDGERNAME>Party</PARTYLEDGERNAME>
                <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>{party_amount}</AMOUNT>
                    <BILLALLOCATIONS.LIST><NAME>FREE-01</NAME><BILLTYPE>Agst Ref</BILLTYPE>
                    <AMOUNT>{party_amount}</AMOUNT></BILLALLOCATIONS.LIST>
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>{bank_amount}</AMOUNT></LEDGERENTRIES.LIST>
            </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#
            );
            let preview = store
                .preview_xml_import(company, payment.as_bytes())
                .unwrap();
            assert!(preview.can_apply(), "{:?}", preview.issues());
            store.apply_xml_import(&preview).unwrap();
            assert!(store
                .outstanding_invoices(company, "2026-06-05")
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn compound_aliases_resolve_without_blocking_unrelated_imports() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Compound aliases").unwrap();
        let units = br#"<ENVELOPE><BODY><DATA><MESSAGE>
            <UNIT><NAME>Box</NAME><ISSIMPLEUNIT>Yes</ISSIMPLEUNIT></UNIT>
            <UNIT><NAME>pcs</NAME><ISSIMPLEUNIT>Yes</ISSIMPLEUNIT></UNIT>
            <UNIT><NAME>Retail box</NAME><ISSIMPLEUNIT>No</ISSIMPLEUNIT>
                <BASEUNITS>Box</BASEUNITS><ADDITIONALUNITS>pcs</ADDITIONALUNITS>
                <CONVERSION>12</CONVERSION></UNIT>
        </MESSAGE></DATA></BODY></ENVELOPE>"#;
        let plan = store.preview_xml_import(company, units).unwrap();
        store.apply_xml_import(&plan).unwrap();
        assert!(store
            .create_stock_unit(company, "Box of 12 pcs", "Duplicate", 0)
            .is_err());
        store
            .create_stock_item(company, "Parts", "Box of 12 pcs")
            .unwrap();
        assert_eq!(store.stock_items(company).unwrap()[0].unit, "pcs");

        // Earlier builds could save a simple unit with the compound's generated name.
        store
            .conn
            .execute(
                "INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places)
             VALUES (?1, 'Box of 12 pcs', 'Historical symbol', 0)",
                [company],
            )
            .unwrap();
        let group = br#"<ENVELOPE><BODY><DATA><MESSAGE>
            <STOCKGROUP><NAME>Equipment</NAME></STOCKGROUP>
        </MESSAGE></DATA></BODY></ENVELOPE>"#;
        let plan = store.preview_xml_import(company, group).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        store
            .create_stock_item(company, "Named stock", "Box of 12 pcs")
            .unwrap();
        let items = store.stock_items(company).unwrap();
        assert_eq!(items[0].unit, "Box of 12 pcs");
        assert_eq!(items[1].unit, "pcs");
    }

    #[test]
    fn imports_billed_quantities_independently_of_stock_movements() {
        for (kind, sign, party_category, trade_category) in [
            ("Sales", 1_i64, Category::Asset, Category::Income),
            ("Purchase", -1, Category::Liability, Category::Expense),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store
                .create_company("Actual and billed quantities")
                .unwrap();
            let party = store
                .create_ledger(company, "Party", party_category)
                .unwrap();
            store
                .create_ledger(company, "Trade", trade_category)
                .unwrap();
            let widget = store.create_stock_item(company, "Widget", "pcs").unwrap();
            let gift = store.create_stock_item(company, "Gift", "pcs").unwrap();
            let north = store.create_stock_location(company, "North").unwrap();
            let south = store.create_stock_location(company, "South").unwrap();
            let main = store
                .stock_locations(company)
                .unwrap()
                .into_iter()
                .find(|row| row.name == "Main")
                .unwrap()
                .id;
            if sign > 0 {
                for (item, location, qty) in [
                    (widget, north, 20000),
                    (widget, south, 30000),
                    (gift, main, 2000),
                ] {
                    let receipt = store
                        .adjust_stock_at(company, item, location, "2026-04-01", qty, "Opening")
                        .unwrap();
                    store
                        .set_stock_receipt_cost(company, receipt, qty / 10, "Opening cost")
                        .unwrap();
                }
            }
            let allocation = |location, actual, billed: i64| {
                format!(
                "<BATCHALLOCATIONS.LIST><GODOWNNAME>{location}</GODOWNNAME><ACTUALQTY>{actual} pcs</ACTUALQTY><BILLEDQTY>{billed} pcs</BILLEDQTY><AMOUNT>{}</AMOUNT></BATCHALLOCATIONS.LIST>",
                sign * 10 * billed,
            )
            };
            let north_row = allocation("North", 10, 10);
            let south_row = allocation("South", 15, 14);
            let free = if sign > 0 {
                "<ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Gift</STOCKITEMNAME><ACTUALQTY>1 pcs</ACTUALQTY><BILLEDQTY>0 pcs</BILLEDQTY></ALLINVENTORYENTRIES.LIST>"
            } else {
                ""
            };
            let item_amount = sign * 240;
            let party_amount = -item_amount;
            let xml = format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
                <VOUCHER><DATE>20260402</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME>
                    <VOUCHERNUMBER>INV</VOUCHERNUMBER><ISINVOICE>Yes</ISINVOICE><PARTYLEDGERNAME>Party</PARTYLEDGERNAME>
                    <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>{party_amount}</AMOUNT>
                        <BILLALLOCATIONS.LIST><NAME>BILL</NAME><BILLTYPE>New Ref</BILLTYPE>
                            <AMOUNT>{party_amount}</AMOUNT>
                        </BILLALLOCATIONS.LIST>
                    </LEDGERENTRIES.LIST>
                    <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Widget</STOCKITEMNAME>
                        <ACTUALQTY>25 pcs</ACTUALQTY><BILLEDQTY>24 pcs</BILLEDQTY>
                        <RATE>10.00/pcs</RATE><AMOUNT>{item_amount}</AMOUNT>
                        <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME>
                            <AMOUNT>{item_amount}</AMOUNT>
                        </ACCOUNTINGALLOCATIONS.LIST>
                        {north_row}{south_row}
                    </ALLINVENTORYENTRIES.LIST>
                    {free}
                </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#
            );
            let unbalanced = xml.replace(&south_row, &allocation("South", 15, 15));
            let preview = store
                .preview_xml_import(company, unbalanced.as_bytes())
                .unwrap();
            assert!(!preview.can_apply());
            assert!(preview
                .issues()
                .iter()
                .any(|issue| issue.contains("location allocations do not match")));
            let preview = store.preview_xml_import(company, xml.as_bytes()).unwrap();
            assert!(preview.can_apply(), "{:?}", preview.issues());
            store.apply_xml_import(&preview).unwrap();
            let invoice = store.invoices(company).unwrap().remove(0);
            assert_eq!((invoice.total, invoice.outstanding), (24000, 24000));
            let line = invoice
                .lines
                .iter()
                .find(|line| line.item == "Widget")
                .unwrap();
            assert_eq!(
                (line.quantity, line.billed_quantity, line.amount),
                (25000, 24000, 24000)
            );
            assert_eq!(
                (
                    line.allocations[1].quantity,
                    line.allocations[1].billed_quantity
                ),
                (15000, 14000)
            );
            let balance = store
                .ledger_register(company, party, None, None)
                .unwrap()
                .closing_balance;
            assert_eq!(balance, i128::from(sign) * 24000);
            let valued = store
                .stock_valuation_at_average_cost(company, "2026-04-02")
                .unwrap();
            assert_eq!(valued.known_value, if sign > 0 { 2600 } else { 24000 });
            let stock = store.stock_balances(company, None).unwrap();
            assert_eq!(
                stock
                    .iter()
                    .find(|row| row.item.id == widget)
                    .unwrap()
                    .quantity,
                25000
            );
            if sign > 0 {
                let gift_line = invoice
                    .lines
                    .iter()
                    .find(|line| line.item == "Gift")
                    .unwrap();
                assert_eq!(
                    (
                        gift_line.quantity,
                        gift_line.billed_quantity,
                        gift_line.amount
                    ),
                    (1000, 0, 0)
                );
                assert_eq!(
                    stock
                        .iter()
                        .find(|row| row.item.id == gift)
                        .unwrap()
                        .quantity,
                    1000
                );
            }
            let mut draft = store.invoice_draft(company, invoice.id).unwrap();
            assert_eq!(draft.lines[0].billed_quantity, Some(24000));
            assert_eq!(
                draft.lines[0].extra_allocations[0].billed_quantity,
                Some(14000)
            );
            draft.narration = "Verified counted and billed quantities".into();
            store.update_invoice(company, invoice.id, &draft).unwrap();
            let revision = store
                .invoice_revisions(company, invoice.id)
                .unwrap()
                .remove(0);
            assert_eq!(revision.lines[0].billed_quantity, 24000);
            assert_eq!(revision.lines[0].allocations[1].billed_quantity, 14000);
            store
                .reverse_invoice(
                    company,
                    invoice.id,
                    "2026-04-03",
                    "REVERSE",
                    "Test reversal",
                )
                .unwrap();
            let stock = store.stock_balances(company, None).unwrap();
            assert_eq!(
                stock
                    .iter()
                    .find(|row| row.item.id == widget)
                    .unwrap()
                    .quantity,
                if sign > 0 { 50000 } else { 0 }
            );
            assert!(store
                .outstanding_invoices(company, "2026-04-03")
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn bills_items_before_stock_moves() {
        for (kind, amount, party_category, trade_category) in [
            ("Sales", 20_i64, Category::Asset, Category::Income),
            ("Purchase", -20, Category::Liability, Category::Expense),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store
                .create_company("Billing before stock movement")
                .unwrap();
            let party = store
                .create_ledger(company, "Party", party_category)
                .unwrap();
            store
                .create_ledger(company, "Trade", trade_category)
                .unwrap();
            let item = store.create_stock_item(company, "Item", "pcs").unwrap();
            let east = store.create_stock_location(company, "East").unwrap();
            let west = store.create_stock_location(company, "West").unwrap();
            let half_amount = amount / 2;
            let opposite = -amount;
            let xml = format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
                   <VOUCHER><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME><ISINVOICE>Yes</ISINVOICE>
                      <DATE>20260402</DATE><VOUCHERNUMBER>BILL-1</VOUCHERNUMBER>
                      <PARTYLEDGERNAME>Party</PARTYLEDGERNAME>
                      <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>{opposite}</AMOUNT>
                          <BILLALLOCATIONS.LIST><NAME>BILL-1</NAME><BILLTYPE>New Ref</BILLTYPE>
                              <AMOUNT>{opposite}</AMOUNT>
                          </BILLALLOCATIONS.LIST>
                      </LEDGERENTRIES.LIST>
                      <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Item</STOCKITEMNAME>
                          <ACTUALQTY>0 pcs</ACTUALQTY><BILLEDQTY>2 pcs</BILLEDQTY>
                          <RATE>10.00/pcs</RATE><AMOUNT>{amount}</AMOUNT>
                          <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>{amount}</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
                          <BATCHALLOCATIONS.LIST>
                              <GODOWNNAME>East</GODOWNNAME><BILLEDQTY>1 pcs</BILLEDQTY>
                              <ACTUALQTY>0 pcs</ACTUALQTY><AMOUNT>{half_amount}</AMOUNT>
                          </BATCHALLOCATIONS.LIST>
                          <BATCHALLOCATIONS.LIST>
                              <GODOWNNAME>West</GODOWNNAME><BILLEDQTY>1 pcs</BILLEDQTY>
                              <ACTUALQTY>0 pcs</ACTUALQTY><AMOUNT>{half_amount}</AMOUNT>
                          </BATCHALLOCATIONS.LIST>
                      </ALLINVENTORYENTRIES.LIST>
                   </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#
            );
            let plan = store.preview_xml_import(company, xml.as_bytes()).unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            store.apply_xml_import(&plan).unwrap();
            let invoice = store.invoices(company).unwrap().remove(0);
            assert_eq!((invoice.total, invoice.outstanding), (2000, 2000));
            assert_eq!(
                (invoice.lines[0].quantity, invoice.lines[0].billed_quantity),
                (0, 2000)
            );
            assert_eq!(
                store
                    .ledger_register(company, party, None, None)
                    .unwrap()
                    .closing_balance,
                i128::from(amount) * 100
            );
            let movements = store.stock_register(company, item, None, None).unwrap();
            assert!(movements.movements.is_empty());
            assert_eq!(
                store
                    .stock_valuation_at_average_cost(company, "2026-04-02")
                    .unwrap()
                    .known_value,
                0
            );
            if kind == "Sales" {
                use crate::actual_cost::{ActualCostIssue, ActualCostMethod};
                store
                    .record_standard_stock_cost(company, item, "2026-04-01", 500, "Fixture rate")
                    .unwrap();
                let standard = store.sales_at_standard_cost(company, None, None).unwrap();
                assert_eq!((standard.net_sales, standard.costed_net_sales), (2000, 0));
                assert_eq!(standard.rows[0].margin(), None);
                for method in ActualCostMethod::ALL {
                    let actual = store
                        .sales_at_actual_cost(company, None, None, method)
                        .unwrap();
                    assert_eq!((actual.net_sales, actual.costed_net_sales), (2000, 0));
                    assert!(matches!(
                        actual.rows[0].issue,
                        Some(ActualCostIssue::PendingStockIssue)
                    ));
                }
            }

            let mut draft = store.invoice_draft(company, invoice.id).unwrap();
            draft.narration = "Billing before physical movement".into();
            store.update_invoice(company, invoice.id, &draft).unwrap();
            let revision = store
                .invoice_revisions(company, invoice.id)
                .unwrap()
                .remove(0);
            assert_eq!(
                (
                    revision.lines[0].quantity,
                    revision.lines[0].billed_quantity
                ),
                (0, 2000)
            );
            assert_eq!(
                (
                    revision.lines[0].allocations[1].quantity,
                    revision.lines[0].allocations[1].billed_quantity
                ),
                (0, 1000)
            );
            store
                .reverse_invoice(
                    company,
                    invoice.id,
                    "2026-04-03",
                    "REVERSE",
                    "Test reversal",
                )
                .unwrap();
            assert!(store
                .stock_register(company, item, None, None)
                .unwrap()
                .movements
                .is_empty());
            assert_eq!(
                store
                    .ledger_register(company, party, None, None)
                    .unwrap()
                    .closing_balance,
                0
            );

            // A second invoice has one shipment and a second allocation billed in advance.
            if kind == "Sales" {
                store
                    .adjust_stock_at(company, item, east, "2026-04-01", 1000, "Opening stock")
                    .unwrap();
            }
            draft.date = "2026-04-04".into();
            draft.reference = "BILL-2".into();
            draft.lines[0].quantity = 1000;
            let mixed_id = store.create_invoice(company, &draft).unwrap();
            for (location, expected) in [(east, 1), (west, 0)] {
                let register = store
                    .stock_register_at(company, item, Some(location), Some("2026-04-04"), None)
                    .unwrap();
                assert_eq!(register.movements.len(), expected);
            }
            store
                .reverse_invoice(
                    company,
                    mixed_id,
                    "2026-04-05",
                    "REVERSE-2",
                    "Test reversal",
                )
                .unwrap();
            assert!(store
                .stock_register_at(company, item, Some(west), None, None)
                .unwrap()
                .movements
                .is_empty());
            assert_eq!(
                store
                    .stock_register(company, item, None, None)
                    .unwrap()
                    .closing_quantity,
                if kind == "Sales" { 1000 } else { 0 }
            );
        }
    }

    #[test]
    fn imports_nested_units_and_keeps_stock_and_invoice_rate_bases() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Compound stock units").unwrap();
        store
            .create_ledger(company, "Buyer", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Trade", Category::Income)
            .unwrap();
        let xml = |body: &str| {
            format!("<ENVELOPE><BODY><DATA><MESSAGE>{body}</MESSAGE></DATA></BODY></ENVELOPE>")
        };
        let masters = xml(
            r#"<UNIT><NAME>Shipping case</NAME><ISSIMPLEUNIT>No</ISSIMPLEUNIT>
                <BASEUNITS>Case</BASEUNITS><ADDITIONALUNITS>Box of 12 pcs</ADDITIONALUNITS>
                <CONVERSION>2</CONVERSION></UNIT>
            <UNIT><NAME>Retail box</NAME><ISSIMPLEUNIT>No</ISSIMPLEUNIT>
                <BASEUNITS>Box</BASEUNITS><ADDITIONALUNITS>pcs</ADDITIONALUNITS>
                <CONVERSION>12</CONVERSION></UNIT>
            <UNIT><NAME>Case</NAME><ISSIMPLEUNIT>Yes</ISSIMPLEUNIT></UNIT>
            <UNIT><NAME>Box</NAME><ISSIMPLEUNIT>Yes</ISSIMPLEUNIT></UNIT>
            <UNIT><NAME>pcs</NAME><ISSIMPLEUNIT>Yes</ISSIMPLEUNIT></UNIT>
            <STOCKITEM><NAME>Kits</NAME><BASEUNITS>Case of 2 Box of 12 pcs</BASEUNITS>
                <OPENINGBALANCE>1-1-2 Case</OPENINGBALANCE>
                <OPENINGRATE>24.00/Case</OPENINGRATE>
                <OPENINGVALUE>38.00</OPENINGVALUE></STOCKITEM>"#,
        );
        for (invalid, issue) in [
            (
                masters.replace("<CONVERSION>2", "<CONVERSION>0"),
                "conversion must be positive",
            ),
            (
                masters.replace(
                    "<ADDITIONALUNITS>Box of 12 pcs",
                    "<ADDITIONALUNITS>Shipping case",
                ),
                "missing or cyclic",
            ),
            (
                masters.replace("1-1-2 Case", "1-1-2.1 Case"),
                "exceeds unit precision",
            ),
        ] {
            let preview = store
                .preview_xml_import_with_opening_date(
                    company,
                    invalid.as_bytes(),
                    Some("2026-04-01"),
                )
                .unwrap();
            assert!(!preview.can_apply());
            assert!(
                preview.issues().iter().any(|error| error.contains(issue)),
                "{:?}",
                preview.issues()
            );
        }
        let preview = store
            .preview_xml_import_with_opening_date(company, masters.as_bytes(), Some("2026-04-01"))
            .unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        let counts = store.apply_xml_import(&preview).unwrap();
        assert_eq!((counts.units, counts.items), (5, 1));
        let item = store.stock_items(company).unwrap().remove(0);
        assert_eq!(item.unit, "pcs");
        let units = store.stock_units(company).unwrap();
        let compound = units.iter().find(|unit| unit.id == item.unit_id).unwrap();
        assert_eq!(compound.symbol, "Shipping case");
        assert_eq!(compound.compound.as_ref().unwrap().tail, "Retail box");
        let valuation = |store: &Store, date| {
            store
                .stock_valuation_at_average_cost(company, date)
                .unwrap()
                .known_value
        };
        assert_eq!(valuation(&store, "2026-04-01"), 3800);
        let stock =
            |store: &Store, date| store.stock_balances(company, Some(date)).unwrap()[0].quantity;
        assert_eq!(stock(&store, "2026-04-01"), 38000);
        let invoice = xml(
            r#"<VOUCHER><DATE>20260402</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
                <VOUCHERNUMBER>INV</VOUCHERNUMBER><ISINVOICE>Yes</ISINVOICE>
                <PARTYLEDGERNAME>Buyer</PARTYLEDGERNAME>
                <LEDGERENTRIES.LIST><LEDGERNAME>Buyer</LEDGERNAME><AMOUNT>-126.25</AMOUNT>
                    <BILLALLOCATIONS.LIST><NAME>BILL</NAME><BILLTYPE>New Ref</BILLTYPE>
                    <AMOUNT>-126.25</AMOUNT></BILLALLOCATIONS.LIST></LEDGERENTRIES.LIST>
                <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Kits</STOCKITEMNAME>
                    <ACTUALQTY>1 Retail box 3 pcs</ACTUALQTY><BILLEDQTY>0-1-3 Case</BILLEDQTY>
                    <RATE>101.00/Retail box</RATE><AMOUNT>126.25</AMOUNT>
                    <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME>
                        <AMOUNT>126.25</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
                </ALLINVENTORYENTRIES.LIST>
            </VOUCHER>"#,
        );
        let preview = store
            .preview_xml_import(company, invoice.as_bytes())
            .unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        store.apply_xml_import(&preview).unwrap();
        let saved = store.invoices(company).unwrap().remove(0);
        let rate_basis = InvoiceRateBasis {
            quantity: 12000,
            unit: "Retail box".into(),
        };
        assert_eq!(saved.total, 12625);
        assert_eq!(saved.lines[0].quantity, 15000);
        assert_eq!(saved.lines[0].unit_price, 10100);
        assert_eq!(saved.lines[0].rate_basis, Some(rate_basis.clone()));
        assert_eq!(saved.lines[0].price_label(), "101.00 / Retail box");
        assert_eq!(stock(&store, "2026-04-02"), 23000);
        assert_eq!(valuation(&store, "2026-04-02"), 2300);
        let mut draft = store.invoice_draft(company, saved.id).unwrap();
        assert_eq!(draft.lines[0].rate_basis, Some(rate_basis.clone()));
        draft.narration = "Rate survives editing other fields".into();
        store.update_invoice(company, saved.id, &draft).unwrap();
        let revisions = store.invoice_revisions(company, saved.id).unwrap();
        assert_eq!(revisions[0].lines[0].rate_basis, Some(rate_basis));
        let note = xml(
            r#"<VOUCHER><DATE>20260403</DATE><VOUCHERTYPENAME>Credit note</VOUCHERTYPENAME>
                <VOUCHERNUMBER>RETURN</VOUCHERNUMBER><ISINVOICE>Yes</ISINVOICE>
                <PARTYLEDGERNAME>Buyer</PARTYLEDGERNAME><REFERENCE>INV</REFERENCE>
                <LEDGERENTRIES.LIST><LEDGERNAME>Buyer</LEDGERNAME><AMOUNT>45.00</AMOUNT>
                    <BILLALLOCATIONS.LIST><NAME>BILL</NAME><BILLTYPE>Agst Ref</BILLTYPE>
                    <AMOUNT>45.00</AMOUNT></BILLALLOCATIONS.LIST></LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>-45.00</AMOUNT></LEDGERENTRIES.LIST>
                <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Kits</STOCKITEMNAME>
                    <ACTUALQTY>6 pcs</ACTUALQTY><BILLEDQTY>0.5 Box</BILLEDQTY>
                    <RATE>90.00/Box</RATE><AMOUNT>-45.00</AMOUNT>
                    <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME>
                        <AMOUNT>-45.00</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
                </ALLINVENTORYENTRIES.LIST>
            </VOUCHER>"#,
        );
        let preview = store.preview_xml_import(company, note.as_bytes()).unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        store.apply_xml_import(&preview).unwrap();
        assert_eq!(stock(&store, "2026-04-03"), 29000);
        assert_eq!(valuation(&store, "2026-04-03"), 2900);
        assert_eq!(
            store.outstanding_invoices(company, "2026-04-03").unwrap()[0].outstanding,
            8125
        );
    }

    #[test]
    fn imports_recorded_invoice_taxes_and_mirrored_batch_locations() {
        for (kind, sign, party_category, trade_category) in [
            ("Sales", 1, Category::Asset, Category::Income),
            ("Purchase", -1, Category::Liability, Category::Expense),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Invoice tax migration").unwrap();
            let party = store
                .create_ledger(company, "Party", party_category)
                .unwrap();
            store
                .create_ledger(company, "Trade", trade_category)
                .unwrap();
            let tax = store
                .create_ledger(company, "Tax", Category::Liability)
                .unwrap();
            let item = store.create_stock_item(company, "Parts", "pcs").unwrap();
            let location = store.create_stock_location(company, "Warehouse").unwrap();
            let batch = store
                .create_stock_batch(company, item, "P1", None, None)
                .unwrap();
            store
                .adjust_stock_batch(
                    company,
                    item,
                    location,
                    Some(batch),
                    "2026-04-01",
                    2000,
                    "Opening",
                )
                .unwrap();

            let (mode, view, inventory_tag, ledger_allocation) = if sign == 1 {
                (
                    "Yes",
                    "Invoice Voucher View",
                    "ALLINVENTORYENTRIES.LIST",
                    "<ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>100</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>",
                )
            } else {
                (
                    "No",
                    "Accounting Voucher View",
                    "INVENTORYALLOCATIONS.LIST",
                    "",
                )
            };
            let party_amount = -sign * 115;
            let stock_amount = sign * 100;
            let tax_amount = sign * 15;
            let inventory = format!(
                r#"<{inventory_tag}>
                <STOCKITEMNAME>Parts</STOCKITEMNAME><RATE>100/pcs</RATE>
                <ACTUALQTY>1 pcs</ACTUALQTY><BILLEDQTY>1 pcs</BILLEDQTY>
                <AMOUNT>{stock_amount}</AMOUNT>{ledger_allocation}
                <BATCHALLOCATIONS.LIST>
                    <GODOWNNAME>Warehouse</GODOWNNAME><BATCHNAME>P1</BATCHNAME>
                    <DESTINATIONGODOWNNAME>Warehouse</DESTINATIONGODOWNNAME>
                    <ACTUALQTY>1 pcs</ACTUALQTY><BILLEDQTY>1 pcs</BILLEDQTY>
                    <AMOUNT>{stock_amount}</AMOUNT>
                </BATCHALLOCATIONS.LIST>
            </{inventory_tag}>"#
            );
            let inventory = if sign == 1 {
                inventory
            } else {
                format!(
                    "<LEDGERENTRIES.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>{stock_amount}</AMOUNT>{inventory}</LEDGERENTRIES.LIST>"
                )
            };
            let xml = format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
                <VOUCHER><DATE>20260402</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME>
                <VOUCHERNUMBER>DOC</VOUCHERNUMBER><ISINVOICE>{mode}</ISINVOICE>
                <PERSISTEDVIEW>{view}</PERSISTEDVIEW><PARTYLEDGERNAME>Party</PARTYLEDGERNAME>
                <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>{party_amount}</AMOUNT>
                    <BILLALLOCATIONS.LIST><NAME>BILL</NAME><BILLTYPE>New Ref</BILLTYPE>
                    <AMOUNT>{party_amount}</AMOUNT></BILLALLOCATIONS.LIST>
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Tax</LEDGERNAME><AMOUNT>{tax_amount}</AMOUNT>
                    <BASICRATEOFINVOICETAX.LIST TYPE="Number">
                        <BASICRATEOFINVOICETAX>14.1250</BASICRATEOFINVOICETAX>
                        <BASICRATEOFINVOICETAX>2</BASICRATEOFINVOICETAX>
                    </BASICRATEOFINVOICETAX.LIST>
                </LEDGERENTRIES.LIST>
                {inventory}</VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#
            );
            for (invalid, issue) in [
                (
                    xml.replace(
                        "<DESTINATIONGODOWNNAME>Warehouse",
                        "<DESTINATIONGODOWNNAME>A different warehouse",
                    ),
                    "batch source and destination locations differ",
                ),
                (
                    xml.replace(">14.1250<", ">unknown<"),
                    "tax rate must be a decimal",
                ),
            ] {
                let preview = store
                    .preview_xml_import(company, invalid.as_bytes())
                    .unwrap();
                assert!(!preview.can_apply());
                assert!(
                    preview.issues().iter().any(|actual| actual.contains(issue)),
                    "{:?}",
                    preview.issues()
                );
                assert!(store.apply_xml_import(&preview).is_err());
                assert!(store.invoices(company).unwrap().is_empty());
            }
            let preview = store.preview_xml_import(company, xml.as_bytes()).unwrap();
            assert!(preview.can_apply(), "{:?}", preview.issues());
            store.apply_xml_import(&preview).unwrap();
            let saved = store.invoices(company).unwrap().remove(0);
            assert_eq!(
                (saved.total, saved.outstanding, saved.tax_amount),
                (11500, 11500, 1500)
            );
            assert_eq!(saved.bill_reference, "BILL");
            assert_eq!(saved.lines[0].allocations[0].location, "Warehouse");
            let rates = &saved.taxes[0].source_rates;
            assert_eq!(
                rates.iter().map(Decimal::to_string).collect::<Vec<_>>(),
                ["14.1250", "2"]
            );
            assert_eq!(
                store
                    .ledger_register(company, party, None, None)
                    .unwrap()
                    .closing_balance,
                i128::from(sign) * 11500,
            );
            assert_eq!(
                store
                    .stock_batch_quantity(company, item, batch, Some(location), None)
                    .unwrap(),
                2000 - sign * 1000,
            );
            let mut draft = store.invoice_draft(company, saved.id).unwrap();
            assert_eq!(draft.additional_taxes[0].source_rates, *rates);
            draft.narration = "Retain source rates when saving remarks".into();
            store.update_invoice(company, saved.id, &draft).unwrap();
            let revisions = store.invoice_revisions(company, saved.id).unwrap();
            assert_eq!(revisions[0].taxes[0].source_rates, *rates);
            let register = store
                .invoice_tax_register(company, None, None, None, Some(tax))
                .unwrap();
            assert_eq!(register.entries[0].source_rates, *rates);
            assert_eq!(register.entries[0].amount, 1500);
            store
                .reverse_invoice(company, saved.id, "2026-04-03", "REVERSE", "Test reversal")
                .unwrap();
            let reversed = store
                .invoice_tax_register(company, None, None, None, Some(tax))
                .unwrap();
            assert_eq!(reversed.entries.len(), 2);
            assert_eq!(reversed.entries[1].amount, -1500);
            assert_eq!(reversed.entries[1].source_rates, *rates);
            assert_eq!(
                store
                    .stock_batch_quantity(company, item, batch, Some(location), None)
                    .unwrap(),
                2000,
            );
        }
    }

    #[test]
    fn imports_sales_and_purchase_returns_into_original_batches_and_bills() {
        for (invoice_kind, note_kind, party_kind, trade_kind, sign) in [
            (
                "Sales",
                "Credit note",
                Category::Asset,
                Category::Income,
                1_i64,
            ),
            (
                "Purchase",
                "Debit note",
                Category::Liability,
                Category::Expense,
                -1,
            ),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Return migration").unwrap();
            let party = store.create_ledger(company, "Party", party_kind).unwrap();
            let trade = store.create_ledger(company, "Trade", trade_kind).unwrap();
            let charge = store.create_ledger(company, "Charge", trade_kind).unwrap();
            let item = store.create_stock_item(company, "Widget", "pcs").unwrap();
            let west = store.create_stock_location(company, "West").unwrap();
            let east = store.create_stock_location(company, "East").unwrap();
            let a = store
                .create_stock_batch(company, item, "A", None, None)
                .unwrap();
            let b = store
                .create_stock_batch(company, item, "B", None, None)
                .unwrap();
            for (location, batch) in [(west, a), (east, b)] {
                store
                    .adjust_stock_batch(
                        company,
                        item,
                        location,
                        Some(batch),
                        "2026-04-01",
                        5000,
                        "Test opening stock",
                    )
                    .unwrap();
            }
            let lines = |ledger, amount: i64, details: String| {
                format!(
                "<LEDGERENTRIES.LIST><LEDGERNAME>{ledger}</LEDGERNAME><AMOUNT>{amount}</AMOUNT>{details}</LEDGERENTRIES.LIST>"
            )
            };
            let bill = |reference, mode, amount: i64| {
                format!(
                "<BILLALLOCATIONS.LIST><NAME>{reference}</NAME><BILLTYPE>{mode}</BILLTYPE><AMOUNT>{amount}</AMOUNT></BILLALLOCATIONS.LIST>"
            )
            };
            let batch = |location, name, qty, amount| {
                format!(
                "<BATCHALLOCATIONS.LIST><GODOWNNAME>{location}</GODOWNNAME><BATCHNAME>{name}</BATCHNAME><ACTUALQTY>{qty} pcs</ACTUALQTY><BILLEDQTY>{qty} pcs</BILLEDQTY><AMOUNT>{amount}</AMOUNT></BATCHALLOCATIONS.LIST>"
            )
            };
            let stock = |tag, qty_each: i64, rate: i64, direction| {
                let quantity = qty_each * 2;
                let total = direction * quantity * rate;
                let once = direction * qty_each * rate;
                let ledger = if tag == "ALLINVENTORYENTRIES.LIST" {
                    format!("<ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>{total}</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>")
                } else {
                    String::new()
                };
                let west = batch("West", "A", qty_each, once);
                let east = batch("East", "B", qty_each, once);
                format!("<{tag}><STOCKITEMNAME>Widget</STOCKITEMNAME><RATE>{rate}/pcs</RATE><ACTUALQTY>{quantity} pcs</ACTUALQTY><BILLEDQTY>{quantity} pcs</BILLEDQTY><AMOUNT>{total}</AMOUNT>{ledger}{west}{east}</{tag}>")
            };
            let voucher = |date, kind, reference, mode, contents| {
                format!(
                "<VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME><VOUCHERNUMBER>{reference}</VOUCHERNUMBER><PARTYLEDGERNAME>Party</PARTYLEDGERNAME><ISINVOICE>{mode}</ISINVOICE>{contents}</VOUCHER>"
            )
            };
            let external =
                "<REFERENCE>EXTERNAL-17</REFERENCE><REFERENCEDATE>20260331</REFERENCEDATE>";
            let invoice = voucher(
                "20260402",
                invoice_kind,
                "INV",
                "Yes",
                lines(
                    "Party",
                    -sign * 44,
                    bill("FIRST", "New Ref", -sign * 15) + &bill("SECOND", "New Ref", -sign * 29),
                ) + &stock("ALLINVENTORYENTRIES.LIST", 2, 10, sign)
                    + &lines("Charge", sign * 4, String::new())
                    + external,
            );
            let external_origin =
                "<ORIGINALINVNO>EXTERNAL-17</ORIGINALINVNO><ORIGINALINVDATE>20260331</ORIGINALINVDATE>";
            let note = |rate| {
                let contents = lines(
                    "Party",
                    sign * (rate * 2 + 2),
                    bill("FIRST", "Agst Ref", sign * 5)
                        + &bill("SECOND", "Agst Ref", sign * (rate * 2 - 3)),
                ) + &lines("Charge", -sign * 2, String::new())
                    + external_origin;
                if sign == 1 {
                    voucher(
                        "20260403",
                        note_kind,
                        "RETURN",
                        "Yes",
                        contents
                            + &lines("Trade", -sign * rate * 2, String::new())
                            + &stock("ALLINVENTORYENTRIES.LIST", 1, rate, -sign),
                    )
                } else {
                    voucher(
                        "20260403",
                        note_kind,
                        "RETURN",
                        "No",
                        contents
                            + &lines(
                                "Trade",
                                -sign * rate * 2,
                                stock("INVENTORYALLOCATIONS.LIST", 1, rate, -sign),
                            ),
                    )
                }
            };
            let xml = |body: &str| {
                format!("<ENVELOPE><BODY><DATA><MESSAGE>{body}</MESSAGE></DATA></BODY></ENVELOPE>")
            };
            let standalone = sign < 0;
            if standalone {
                let input = xml(&invoice);
                let plan = store.preview_xml_import(company, input.as_bytes()).unwrap();
                assert!(plan.can_apply(), "{:?}", plan.issues());
                store.apply_xml_import(&plan).unwrap();
            }
            let input = |rate| {
                if standalone {
                    xml(&note(rate))
                } else {
                    xml(&(note(rate) + &invoice))
                }
            };
            let wrong_date = input(10).replace(
                "<ORIGINALINVDATE>20260331</ORIGINALINVDATE>",
                "<ORIGINALINVDATE>20260402</ORIGINALINVDATE>",
            );
            assert!(store
                .preview_xml_import(company, wrong_date.as_bytes())
                .unwrap()
                .issues()
                .iter()
                .any(|issue| issue.contains("original stock invoice EXTERNAL-17 is unavailable")));
            let premature = input(10).replace("<DATE>20260403</DATE>", "<DATE>20260401</DATE>");
            assert!(store
                .preview_xml_import(company, premature.as_bytes())
                .unwrap()
                .issues()
                .iter()
                .any(|issue| issue.contains("return precedes its original invoice")));
            let second_invoice = invoice
                .replace(">INV<", ">INV-SECOND<")
                .replace(">FIRST<", ">OTHER-FIRST<")
                .replace(">SECOND<", ">OTHER-SECOND<")
                .replace(">20260331<", ">20260330<");
            let ambiguous_note =
                note(10).replace("<ORIGINALINVDATE>20260331</ORIGINALINVDATE>", "");
            let mut ambiguous = ambiguous_note + &second_invoice;
            if !standalone {
                ambiguous.push_str(&invoice);
            }
            assert!(store
                .preview_xml_import(company, xml(&ambiguous).as_bytes())
                .unwrap()
                .issues()
                .iter()
                .any(|issue| issue
                    .contains("original stock invoice EXTERNAL-17 is unavailable or ambiguous")));
            let mut different_invoice = note(10).replace(
                "<ORIGINALINVDATE>20260331</ORIGINALINVDATE>",
                "<ORIGINALINVDATE>20260330</ORIGINALINVDATE>",
            ) + &second_invoice;
            if !standalone {
                different_invoice.push_str(&invoice);
            }
            assert!(store
                .preview_xml_import(company, xml(&different_invoice).as_bytes())
                .unwrap()
                .issues()
                .iter()
                .any(|issue| issue
                    .contains("belongs to a different invoice than the returned stock")));
            if standalone {
                let saved = store
                    .invoice_register(company, None, None, None, None, "external-17")
                    .unwrap()
                    .remove(0);
                let original = store.invoice_draft(company, saved.id).unwrap();
                assert_eq!(original.external_reference, saved.external_reference);
                let mut revised = original.clone();
                revised.external_reference.number = Some("SUPPLIER-EDIT".into());
                let stale = store
                    .preview_xml_import(company, input(10).as_bytes())
                    .unwrap();
                assert!(stale.can_apply(), "{:?}", stale.issues());
                store.update_invoice(company, saved.id, &revised).unwrap();
                assert!(store.apply_xml_import(&stale).is_err());
                assert_eq!(store.vouchers(company).unwrap().len(), 1);
                assert_eq!(
                    store.invoice_revisions(company, saved.id).unwrap()[0].external_reference,
                    original.external_reference
                );
                store.update_invoice(company, saved.id, &original).unwrap();
            }
            let wrong_location = input(10).replacen(
                "<GODOWNNAME>West</GODOWNNAME><BATCHNAME>A</BATCHNAME>",
                "<GODOWNNAME>East</GODOWNNAME><BATCHNAME>A</BATCHNAME>",
                1,
            );
            let plan = store
                .preview_xml_import(company, wrong_location.as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            assert!(store
                .apply_xml_import(&plan)
                .unwrap_err()
                .to_string()
                .contains("Returned stock does not match an available allocation"));
            assert_eq!(
                store.vouchers(company).unwrap().len(),
                usize::from(standalone)
            );
            let plan = store
                .preview_xml_import(company, input(10).as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            assert_eq!(store.apply_xml_import(&plan).unwrap().bill_notes, 1);
            let invoice = store.invoices(company).unwrap().remove(0);
            assert_eq!(invoice.reference, "INV");
            assert_eq!(invoice.date, "2026-04-02");
            assert_eq!(
                invoice.external_reference.number.as_deref(),
                Some("EXTERNAL-17")
            );
            assert_eq!(
                invoice.external_reference.date.as_deref(),
                Some("2026-03-31")
            );
            let saved = store
                .invoice_credit_notes(company, invoice.id)
                .unwrap()
                .remove(0);
            let returned = store.invoice_return_lines(company, saved.id).unwrap();
            assert_eq!(returned.len(), 2);
            assert_eq!(
                saved.bill_allocations,
                [("FIRST".into(), 500), ("SECOND".into(), 1700)]
            );
            let check = |s: &Store, date, expected_stock, expected_owed, expected_charge| {
                for (location, batch) in [(west, a), (east, b)] {
                    assert_eq!(
                        s.stock_batch_quantity(company, item, batch, Some(location), Some(date))
                            .unwrap(),
                        expected_stock
                    );
                }
                let balance = |ledger| {
                    s.ledger_register(company, ledger, None, Some(date))
                        .unwrap()
                        .closing_balance
                };
                assert_eq!(balance(party), i128::from(sign) * expected_owed);
                assert_eq!(balance(charge), -i128::from(sign) * expected_charge);
                assert_eq!(balance(trade) + balance(charge), -balance(party));
                assert_eq!(
                    s.outstanding_invoices(company, date)
                        .unwrap()
                        .iter()
                        .map(|row| row.outstanding)
                        .sum::<i128>(),
                    expected_owed
                );
            };
            check(
                &store,
                "2026-04-02",
                i128::from(5000 - sign * 2000),
                4400,
                400,
            );
            check(
                &store,
                "2026-04-03",
                i128::from(5000 - sign * 1000),
                2200,
                200,
            );
            store
                .reverse_invoice_credit(company, saved.id, "2026-04-04", "UNDO", "Reverse return")
                .unwrap();
            check(
                &store,
                "2026-04-04",
                i128::from(5000 - sign * 2000),
                4400,
                400,
            );
            check(
                &store,
                "2026-04-03",
                i128::from(5000 - sign * 1000),
                2200,
                200,
            );
            let bank = store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            let returned_amount = 18;
            let named_amount = if sign == 1 {
                returned_amount
            } else {
                returned_amount - 4
            };
            let applied_amount = returned_amount - named_amount;
            let settlement = invoice
                .bill_splits
                .iter()
                .map(|bill| BillSettlementAllocation {
                    target: BillSettlementTarget {
                        invoice_id: invoice.id,
                        split_id: Some(bill.id),
                    },
                    amount: bill.amount
                        - if bill.reference == "SECOND" {
                            applied_amount * 100
                        } else {
                            0
                        },
                })
                .collect::<Vec<_>>();
            let (settlement_voucher, _) = store
                .settle_bills(company, bank, "2026-04-05", "SETTLED", &settlement)
                .unwrap();
            let due = if applied_amount > 0 {
                bill("SECOND", "Agst Ref", sign * applied_amount)
            } else {
                String::new()
            };
            let returned_stock = lines(
                "Party",
                sign * returned_amount,
                due + &bill("CARRY", "New Ref", sign * named_amount),
            ) + &lines("Trade", -sign * 16, String::new())
                + &lines("Charge", -sign * 2, String::new())
                + &stock("ALLINVENTORYENTRIES.LIST", 1, 8, -sign);
            let origin = if sign == 1 {
                "<REFERENCE>INV</REFERENCE><REFERENCEDATE>20260402</REFERENCEDATE>"
            } else {
                external_origin
            };
            let return_with_credit = voucher(
                "20260406",
                note_kind,
                "RETURN-CREDIT",
                "Yes",
                format!("{origin}{returned_stock}"),
            );
            let wrong_day = return_with_credit
                .replace("20260402", "20260401")
                .replace("20260331", "20260402");
            let invalid = store
                .preview_xml_import(company, xml(&wrong_day).as_bytes())
                .unwrap();
            assert!(invalid
                .issues()
                .iter()
                .any(|issue| issue.contains("original stock invoice")
                    && issue.contains("is unavailable")));
            let later = voucher(
                "20260407",
                invoice_kind,
                "FUNDED",
                "Yes",
                lines(
                    "Party",
                    -sign * named_amount,
                    bill("CARRY", "Agst Ref", -sign * named_amount),
                ) + &lines("Trade", sign * named_amount, String::new()),
            );
            let input = xml(&(later + &return_with_credit));
            let plan = store.preview_xml_import(company, input.as_bytes()).unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (
                    counts.bill_notes,
                    counts.note_credits,
                    counts.credit_applications
                ),
                (1, 1, 1)
            );
            let credit = store.bill_note_credits(company, None).unwrap().remove(0);
            assert_eq!(credit.available, 0);
            let history = store
                .imported_stock_return_lines(company, invoice.id)
                .unwrap();
            assert_eq!(history.len(), 2);
            assert!(history.iter().all(|row| row.stock.net_amount == 800));
            assert!(history
                .iter()
                .all(|row| row.voucher_id == credit.voucher_id));
            let at_return = store
                .bill_note_credits(company, Some("2026-04-06"))
                .unwrap();
            assert_eq!(at_return[0].available, 100 * i128::from(named_amount));
            assert!(store
                .outstanding_invoices(company, "2026-04-07")
                .unwrap()
                .is_empty());
            let cash = store
                .ledger_register(company, bank, None, Some("2026-04-07"))
                .unwrap()
                .closing_balance;
            assert_eq!(cash, i128::from(sign * (44 - applied_amount) * 100));
            assert!(store
                .reverse_voucher(
                    company,
                    credit.voucher_id,
                    "2026-04-08",
                    "EARLY",
                    "Credit still applied"
                )
                .is_err());
            let application = store
                .bill_note_credit_applications(company, credit.id)
                .unwrap()[0]
                .id;
            store
                .release_bill_note_credit(
                    company,
                    application,
                    "2026-04-08",
                    "Restore credit before reversal",
                )
                .unwrap();
            assert!(store
                .reverse_voucher(
                    company,
                    credit.voucher_id,
                    "2026-04-07",
                    "EARLIER",
                    "Release dated later"
                )
                .is_err());
            store
                .reverse_voucher(
                    company,
                    credit.voucher_id,
                    "2026-04-09",
                    "REVERSE-CREDIT-RETURN",
                    "Undo credit and stock",
                )
                .unwrap();
            assert_eq!(
                store.bill_note_credits(company, None).unwrap()[0].available,
                0
            );
            assert_eq!(
                store
                    .outstanding_invoices(company, "2026-04-09")
                    .unwrap()
                    .iter()
                    .map(|row| row.outstanding)
                    .sum::<i128>(),
                i128::from(returned_amount) * 100
            );
            assert_eq!(
                store
                    .ledger_register(company, party, None, Some("2026-04-09"))
                    .unwrap()
                    .closing_balance,
                i128::from(returned_amount * sign) * 100
            );
            for (location, batch) in [(west, a), (east, b)] {
                assert_eq!(
                    store
                        .stock_batch_quantity(
                            company,
                            item,
                            batch,
                            Some(location),
                            Some("2026-04-06")
                        )
                        .unwrap(),
                    i128::from(5000 - sign * 1000)
                );
                assert_eq!(
                    store
                        .stock_batch_quantity(
                            company,
                            item,
                            batch,
                            Some(location),
                            Some("2026-04-09")
                        )
                        .unwrap(),
                    i128::from(5000 - sign * 2000)
                );
            }
            store
                .reverse_settlement(
                    company,
                    settlement_voucher,
                    "2026-04-10",
                    "UNDO-PAYMENT",
                    "Restore original bills",
                )
                .unwrap();
            let repriced = note(11).replace("20260403", "20260411").replace(
                "<VOUCHERNUMBER>RETURN</VOUCHERNUMBER>",
                "<VOUCHERNUMBER>DIFFERENT-AMOUNT</VOUCHERNUMBER>",
            );
            let input = xml(&repriced);
            let plan = store.preview_xml_import(company, input.as_bytes()).unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            assert_eq!(store.apply_xml_import(&plan).unwrap().bill_notes, 1);
            let returned = store
                .imported_stock_return_lines(company, invoice.id)
                .unwrap();
            let lines = returned
                .iter()
                .filter(|row| row.reference == "DIFFERENT-AMOUNT")
                .collect::<Vec<_>>();
            assert_eq!(lines.len(), 2);
            assert!(lines
                .iter()
                .all(|row| row.stock.quantity == 1000 && row.stock.net_amount == 1100));
            let open = store
                .outstanding_invoices(company, "2026-04-11")
                .unwrap()
                .into_iter()
                .filter(|row| row.invoice_id == invoice.id)
                .map(|row| row.outstanding)
                .sum::<i128>();
            assert_eq!(open, 2000);
            store
                .reverse_voucher(
                    company,
                    lines[0].voucher_id,
                    "2026-04-12",
                    "UNDO-AMOUNT",
                    "Reverse different return amount",
                )
                .unwrap();
            let open = store
                .outstanding_invoices(company, "2026-04-12")
                .unwrap()
                .into_iter()
                .filter(|row| row.invoice_id == invoice.id)
                .map(|row| row.outstanding)
                .sum::<i128>();
            assert_eq!(open, 4400);
        }
    }

    #[test]
    fn named_refunds_reconcile_cash_credit_and_combined_vouchers() {
        for (note, opposite_invoice, cash_kind, party_category, trade_category, sign) in [
            (
                "Credit note",
                "Purchase",
                "Payment",
                Category::Asset,
                Category::Expense,
                1_i64,
            ),
            (
                "Debit note",
                "Sales",
                "Receipt",
                Category::Liability,
                Category::Income,
                -1,
            ),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Refund migration").unwrap();
            let party = store
                .create_ledger(company, "Party", party_category)
                .unwrap();
            let bank = store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            let discount = store
                .create_ledger(company, "Discount", Category::Expense)
                .unwrap();
            store
                .create_ledger(company, "Trade", trade_category)
                .unwrap();
            let bill = |reference, mode, amount: i64| {
                format!(
                "<BILLALLOCATIONS.LIST><NAME>{reference}</NAME><BILLTYPE>{mode}</BILLTYPE><AMOUNT>{amount}</AMOUNT></BILLALLOCATIONS.LIST>"
            )
            };
            let line = |ledger, amount: i64, bills: String| {
                format!(
                "<ALLLEDGERENTRIES.LIST><LEDGERNAME>{ledger}</LEDGERNAME><AMOUNT>{amount}</AMOUNT>{bills}</ALLLEDGERENTRIES.LIST>"
            )
            };
            let voucher = |date, kind, reference, flags, lines| {
                format!(
                "<VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME><VOUCHERNUMBER>{reference}</VOUCHERNUMBER>{flags}{lines}</VOUCHER>"
            )
            };
            let xml = |body: &str| {
                format!("<ENVELOPE><BODY><DATA><MESSAGE>{body}</MESSAGE></DATA></BODY></ENVELOPE>")
            };
            let flags = "<ISINVOICE>Yes</ISINVOICE><PARTYLEDGERNAME>Party</PARTYLEDGERNAME>";
            let note = voucher(
                "20260201",
                note,
                "NOTE",
                flags,
                line(
                    "Party",
                    sign * 50,
                    bill("A", "New Ref", sign * 30) + &bill("B", "New Ref", sign * 20),
                ) + &line("Trade", -sign * 50, String::new()),
            );
            let invoice = voucher(
                "20260202",
                opposite_invoice,
                "OPP-1",
                flags,
                line("Party", sign * 20, bill("OPEN", "New Ref", sign * 20))
                    + &line("Trade", -sign * 20, String::new()),
            );
            let cash = |date, reference, gross, deduction, bills| {
                let discount_line = if deduction != 0 {
                    line("Discount", sign * deduction, String::new())
                } else {
                    String::new()
                };
                voucher(
                    date,
                    cash_kind,
                    reference,
                    "",
                    line("Party", -sign * gross, bills)
                        + &line("Bank", sign * (gross - deduction), String::new())
                        + &discount_line,
                )
            };
            let combined = cash(
                "20260204",
                "MIXED",
                30,
                2,
                bill("OPEN", "Agst Ref", -sign * 10)
                    + &bill("A", "Agst Ref", -sign * 12)
                    + &bill("B", "Agst Ref", -sign * 3)
                    + &bill("DEPOSIT", "Advance", -sign * 5),
            );
            let overdrawn = cash(
                "20260204",
                "TOO-MUCH",
                31,
                0,
                bill("A", "Agst Ref", -sign * 31),
            );
            let wrong_discount = cash(
                "20260204",
                "WRONG-DISCOUNT",
                5,
                1,
                bill("A", "Agst Ref", -sign * 5),
            );
            let ambiguous_invoice = invoice.replace("<NAME>OPEN</NAME>", "<NAME>A</NAME>");
            let ambiguous_refund = cash(
                "20260204",
                "AMBIGUOUS",
                5,
                0,
                bill("A", "Agst Ref", -sign * 5),
            );
            for (body, issue) in [
                (
                    format!("{note}{overdrawn}"),
                    "exceeds the available amount on credit",
                ),
                (
                    format!("{note}{wrong_discount}"),
                    "discounts cannot exceed the amount allocated to bills",
                ),
                (
                    format!("{note}{ambiguous_invoice}{ambiguous_refund}"),
                    "matches both a bill and a refund credit",
                ),
            ] {
                let plan = store
                    .preview_xml_import(company, xml(&body).as_bytes())
                    .unwrap();
                assert!(
                    plan.issues().iter().any(|found| found.contains(issue)),
                    "{:?}",
                    plan.issues()
                );
                assert!(store.apply_xml_import(&plan).is_err());
            }
            assert!(store.vouchers(company).unwrap().is_empty());
            let plan = store
                .preview_xml_import(
                    company,
                    xml(&format!("{combined}{note}{invoice}")).as_bytes(),
                )
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (counts.settlements, counts.advances, counts.note_refunds),
                (1, 1, 2)
            );
            assert_eq!(store.vouchers(company).unwrap().len(), 3);
            let credit = store.bill_note_credits(company, None).unwrap();
            let refunds = store.bill_note_refunds(company, credit[0].id).unwrap();
            let mixed = refunds[0].voucher_id;
            assert_eq!(refunds[0].amount, 1200);
            assert_eq!(
                store.bill_note_refunds(company, credit[1].id).unwrap()[0].voucher_id,
                mixed
            );
            let advance = store.party_advances(company, None).unwrap().remove(0);
            assert_eq!(advance.voucher_id, mixed);
            assert!(advance.includes_settlements && advance.includes_refunds);
            let open_bill = store
                .outstanding_invoices(company, "2026-02-04")
                .unwrap()
                .remove(0);
            assert_eq!(
                store
                    .invoice_settlements(company, open_bill.invoice_id)
                    .unwrap()[0]
                    .voucher_id,
                mixed
            );
            let reconcile = |store: &Store, date, open, advance, credit, cash, discount_amount| {
                let available = store
                    .bill_note_credits(company, Some(date))
                    .unwrap()
                    .iter()
                    .map(|row| row.available)
                    .sum::<i128>();
                let outstanding = store
                    .outstanding_invoices(company, date)
                    .unwrap()
                    .iter()
                    .map(|row| row.outstanding)
                    .sum::<i128>();
                let advances = store
                    .party_advances(company, Some(date))
                    .unwrap()
                    .iter()
                    .map(|row| row.available)
                    .sum::<i128>();
                assert_eq!((outstanding, advances, available), (open, advance, credit));
                let balance = |id| {
                    store
                        .ledger_register(company, id, None, Some(date))
                        .unwrap()
                        .closing_balance
                };
                assert_eq!(balance(party), (advance - open - credit) * i128::from(sign));
                assert_eq!(balance(bank), cash * i128::from(sign));
                assert_eq!(balance(discount), discount_amount * i128::from(sign));
            };
            reconcile(&store, "2026-02-04", 1000, 500, 3500, -2800, -200);
            assert!(store
                .reverse_voucher(
                    company,
                    credit[0].voucher_id,
                    "2026-02-05",
                    "VOID-NOTE",
                    "Outstanding refunds"
                )
                .is_err());
            assert!(store
                .reverse_voucher(company, mixed, "2026-02-05", "VOID-MIXED", "Wrong route")
                .is_err());
            if sign == 1 {
                store
                    .reverse_settlement(
                        company,
                        mixed,
                        "2026-02-06",
                        "REVERSE",
                        "Undo combined payment",
                    )
                    .unwrap();
            } else {
                store
                    .reverse_party_advance(
                        company,
                        advance.id,
                        "2026-02-06",
                        "REVERSE",
                        "Undo combined receipt",
                    )
                    .unwrap();
            }
            reconcile(&store, "2026-02-06", 2000, 0, 5000, 0, 0);
            reconcile(&store, "2026-02-04", 1000, 500, 3500, -2800, -200);
            let stale_xml = xml(&cash(
                "20260207",
                "STALE",
                5,
                0,
                bill("A", "Agst Ref", -sign * 5),
            ));
            let stale = store
                .preview_xml_import(company, stale_xml.as_bytes())
                .unwrap();
            assert!(stale.can_apply(), "{:?}", stale.issues());
            let native = store
                .refund_bill_note_credits(
                    company,
                    bank,
                    "2026-02-07",
                    "NATIVE",
                    &[
                        NoteRefundAllocation {
                            credit_id: credit[0].id,
                            amount: 500,
                        },
                        NoteRefundAllocation {
                            credit_id: credit[1].id,
                            amount: 700,
                        },
                    ],
                )
                .unwrap();
            assert!(store
                .apply_xml_import(&stale)
                .unwrap_err()
                .to_string()
                .contains("changed since preview"));
            reconcile(&store, "2026-02-07", 2000, 0, 3800, -1200, 0);
            store
                .reverse_voucher(
                    company,
                    native,
                    "2026-02-08",
                    "REVERSE-NATIVE",
                    "Undo refund",
                )
                .unwrap();
            reconcile(&store, "2026-02-08", 2000, 0, 5000, 0, 0);
            let later = xml(&cash(
                "20260209",
                "REFUND-ADVANCE",
                10,
                0,
                bill("B", "Agst Ref", -sign * 7) + &bill("DEPOSIT-2", "Advance", -sign * 3),
            ));
            let plan = store.preview_xml_import(company, later.as_bytes()).unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (counts.settlements, counts.advances, counts.note_refunds),
                (0, 1, 1)
            );
            reconcile(&store, "2026-02-09", 2000, 300, 4300, -1000, 0);
            let advance = store
                .party_advances(company, None)
                .unwrap()
                .into_iter()
                .find(|row| row.reference == "REFUND-ADVANCE")
                .unwrap();
            store
                .reverse_party_advance(
                    company,
                    advance.id,
                    "2026-02-10",
                    "REVERSE-ADVANCE",
                    "Undo refund and advance",
                )
                .unwrap();
            reconcile(&store, "2026-02-10", 2000, 0, 5000, 0, 0);
            let pure = xml(&cash(
                "20260211",
                "REFUND-ONLY",
                6,
                0,
                bill("A", "Agst Ref", -sign * 6),
            ));
            let plan = store.preview_xml_import(company, pure.as_bytes()).unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (counts.settlements, counts.advances, counts.note_refunds),
                (0, 0, 1)
            );
            reconcile(&store, "2026-02-11", 2000, 0, 4400, -600, 0);
            let (invoice_kind, account_kind) = if sign == 1 {
                ("Sales", Category::Income)
            } else {
                ("Purchase", Category::Expense)
            };
            store
                .create_ledger(company, "Credit trade", account_kind)
                .unwrap();
            let using_credit = |amount| {
                xml(&voucher(
                    "20260212",
                    invoice_kind,
                    "USE-CREDIT",
                    flags,
                    line(
                        "Party",
                        -sign * amount,
                        bill("A", "Agst Ref", -sign * amount),
                    ) + &line("Credit trade", sign * amount, String::new()),
                ))
            };
            let excess = using_credit(25);
            let invalid = store
                .preview_xml_import(company, excess.as_bytes())
                .unwrap();
            assert!(invalid
                .issues()
                .iter()
                .any(|issue| issue.contains("exceeds the available amount on credit A")));
            let remaining = using_credit(24);
            let plan = store
                .preview_xml_import(company, remaining.as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            assert_eq!(
                store.apply_xml_import(&plan).unwrap().credit_applications,
                1
            );
            reconcile(&store, "2026-02-12", 2000, 0, 2000, -600, 0);
            let advance_kind = if sign == 1 { "Receipt" } else { "Payment" };
            let named_advance = |reference| {
                voucher(
                    "20260213",
                    advance_kind,
                    reference,
                    "",
                    line("Party", sign, bill("OPEN", "Advance", sign))
                        + &line("Bank", -sign, String::new()),
                )
            };
            let regular_cash = cash(
                "20260214",
                "REGULAR",
                5,
                0,
                bill("OPEN", "Agst Ref", -sign * 5),
            );
            let inputs = xml(&format!(
                "{}{}{regular_cash}",
                named_advance("DUP-1"),
                named_advance("DUP-2")
            ));
            let plan = store
                .preview_xml_import(company, inputs.as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (counts.settlements, counts.advances, counts.note_refunds),
                (1, 2, 0)
            );
        }
    }

    #[test]
    fn named_note_credits_fund_later_bills_without_cash_and_support_release() {
        for (kind, invoice_kind, note_kind, party_category, trade_category, sign) in [
            (
                InvoiceKind::Sale,
                "Sales",
                "Credit note",
                Category::Asset,
                Category::Income,
                1_i64,
            ),
            (
                InvoiceKind::Purchase,
                "Purchase",
                "Debit note",
                Category::Liability,
                Category::Expense,
                -1,
            ),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Credit migration").unwrap();
            let party = store
                .create_ledger(company, "Party", party_category)
                .unwrap();
            let bank = store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            store
                .create_ledger(company, "Trade", trade_category)
                .unwrap();
            let bill = |reference, mode, signed_amount, terms| {
                format!(
                "<BILLALLOCATIONS.LIST><NAME>{reference}</NAME><BILLTYPE>{mode}</BILLTYPE><AMOUNT>{signed_amount}</AMOUNT>{terms}</BILLALLOCATIONS.LIST>"
            )
            };
            let voucher = |date, kind, reference, signed_amount: i64, bills| {
                format!(
                    r#"
                <VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME><VOUCHERNUMBER>{reference}</VOUCHERNUMBER>
                 <ISINVOICE>Yes</ISINVOICE><PARTYLEDGERNAME>Party</PARTYLEDGERNAME>
                 <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>{signed_amount}</AMOUNT>{bills}</LEDGERENTRIES.LIST>
                 <LEDGERENTRIES.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>{trade}</AMOUNT></LEDGERENTRIES.LIST>
                </VOUCHER>"#,
                    trade = -signed_amount
                )
            };
            let envelope = |body: &str| {
                format!("<ENVELOPE><BODY><DATA><MESSAGE>{body}</MESSAGE></DATA></BODY></ENVELOPE>")
            };
            let original = voucher(
                "20260101",
                invoice_kind,
                "ORIGINAL",
                -sign * 50,
                bill("ORIGINAL", "New Ref", -sign * 50, ""),
            );
            let terms = "<BILLCREDITPERIOD>30 Days</BILLCREDITPERIOD>";
            let note = voucher(
                "20260102",
                note_kind,
                "NOTE",
                sign * 60,
                bill("ORIGINAL", "Agst Ref", sign * 20, "")
                    + &bill("CREDIT-A", "New Ref", sign * 30, terms)
                    + &bill("CREDIT-B", "New Ref", sign * 10, ""),
            );
            let later = voucher(
                "20260104",
                invoice_kind,
                "LATER",
                -sign * 45,
                bill("UNPAID", "New Ref", -sign * 15, "")
                    + &bill("CREDIT-A", "Agst Ref", -sign * 20, terms)
                    + &bill("CREDIT-B", "Agst Ref", -sign * 10, ""),
            );
            let last = |amount| {
                voucher(
                    "20260106",
                    invoice_kind,
                    "FINAL",
                    -sign * amount,
                    bill("CREDIT-A", "Agst Ref", -sign * amount, terms),
                )
            };
            let masters = envelope(&format!("{original}{note}"));
            for (invalid, issue) in [
                (
                    format!("{}{later}", last(11)),
                    "exceeds the available amount on credit",
                ),
                (later.replace("20260104", "20260101"), "precedes credit"),
                (later.replace("30 Days", "31 Days"), "Credit terms differ"),
                (
                    format!(
                        "{later}{}",
                        note.replace("<VOUCHERNUMBER>NOTE<", "<VOUCHERNUMBER>NOTE-2<")
                    ),
                    "named credit CREDIT-A is ambiguous",
                ),
            ] {
                let vouchers = envelope(&invalid);
                let plan = store
                    .preview_xml_import_batch(
                        company,
                        &[masters.as_bytes(), vouchers.as_bytes()],
                        None,
                    )
                    .unwrap();
                assert!(
                    plan.issues().iter().any(|found| found.contains(issue)),
                    "{:?}",
                    plan.issues()
                );
                assert!(store.apply_xml_import(&plan).is_err());
            }
            let vouchers = envelope(&format!("{later}{}", last(10)));
            let plan = store
                .preview_xml_import_batch(company, &[vouchers.as_bytes(), masters.as_bytes()], None)
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (
                    counts.invoices,
                    counts.bill_notes,
                    counts.note_credits,
                    counts.credit_applications,
                    counts.advance_applications
                ),
                (3, 1, 2, 3, 0)
            );
            assert_eq!(store.vouchers(company).unwrap().len(), 4);
            let credits = store.bill_note_credits(company, None).unwrap();
            assert_eq!(credits.len(), 2);
            assert!(credits.iter().all(|row| row.kind == kind
                && row.available == 0
                && row.voucher_id == credits[0].voucher_id));
            assert_eq!(credits[0].credit_days, Some(30));
            assert!(store.is_bill_note(company, credits[0].voucher_id).unwrap());
            let ledger = |s: &Store, id, date| {
                s.ledger_register(company, id, None, Some(date))
                    .unwrap()
                    .closing_balance
            };
            let reconcile = |store: &Store, date, open, credit| {
                let owed = store
                    .outstanding_invoices(company, date)
                    .unwrap()
                    .iter()
                    .map(|row| row.outstanding)
                    .sum::<i128>();
                let available = store
                    .bill_note_credits(company, Some(date))
                    .unwrap()
                    .iter()
                    .map(|row| row.available)
                    .sum::<i128>();
                assert_eq!((owed, available), (open, credit));
                assert_eq!(
                    ledger(store, party, date),
                    (owed - available) * i128::from(sign)
                );
                assert_eq!(ledger(store, bank, date), 0);
            };
            reconcile(&store, "2026-01-02", 3000, 4000);
            reconcile(&store, "2026-01-04", 4500, 1000);
            reconcile(&store, "2026-01-06", 4500, 0);
            let credit_a = credits[0].id;
            let note_id = credits[0].voucher_id;
            let history = store
                .bill_note_credit_applications(company, credit_a)
                .unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|row| (row.invoice_reference.as_str(), row.amount))
                    .collect::<Vec<_>>(),
                [("LATER", 2000), ("FINAL", 1000)]
            );
            assert!(store
                .reverse_voucher(company, note_id, "2026-01-07", "REV", "Wrong note")
                .is_err());
            assert!(store
                .release_bill_note_credit(company, history[0].id, "2026-01-03", "Too early")
                .is_err());
            store
                .release_bill_note_credit(
                    company,
                    history[0].id,
                    "2026-01-08",
                    "Release migrated application",
                )
                .unwrap();
            reconcile(&store, "2026-01-08", 6500, 2000);
            let unpaid = store
                .outstanding_invoices(company, "2026-01-08")
                .unwrap()
                .into_iter()
                .find(|row| row.bill_reference == "UNPAID")
                .unwrap();
            let target = BillSettlementTarget {
                invoice_id: unpaid.invoice_id,
                split_id: unpaid.split_id,
            };
            let application = store
                .apply_bill_note_credit(company, credit_a, target, "2026-01-09", 1500)
                .unwrap();
            reconcile(&store, "2026-01-09", 5000, 500);
            assert_eq!(store.vouchers(company).unwrap().len(), 4);
            store
                .release_bill_note_credit(
                    company,
                    application,
                    "2026-01-10",
                    "Release manual application",
                )
                .unwrap();
            store
                .release_bill_note_credit(
                    company,
                    history[1].id,
                    "2026-01-10",
                    "Release last application",
                )
                .unwrap();
            let credit_b_use = store
                .bill_note_credit_applications(company, credits[1].id)
                .unwrap()[0]
                .id;
            store
                .release_bill_note_credit(
                    company,
                    credit_b_use,
                    "2026-01-11",
                    "Release second credit",
                )
                .unwrap();
            assert!(store
                .reverse_voucher(company, note_id, "2026-01-10", "REV", "Too early")
                .is_err());
            store
                .reverse_voucher(
                    company,
                    note_id,
                    "2026-01-12",
                    "REV",
                    "Reverse note after releases",
                )
                .unwrap();
            reconcile(&store, "2026-01-12", 10500, 0);
            reconcile(&store, "2026-01-04", 4500, 1000);
            let standalone = voucher(
                "20260113",
                note_kind,
                "CREDIT-ONLY",
                sign * 25,
                bill("CREDIT-C", "New Ref", sign * 25, ""),
            );
            let plan = store
                .preview_xml_import(company, envelope(&standalone).as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            assert_eq!(store.apply_xml_import(&plan).unwrap().note_credits, 1);
            reconcile(&store, "2026-01-13", 10500, 2500);
            let separate = voucher(
                "20260114",
                invoice_kind,
                "SEPARATE",
                -sign * 25,
                bill("CREDIT-C", "Agst Ref", -sign * 25, ""),
            );
            let plan = store
                .preview_xml_import(company, envelope(&separate).as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            assert_eq!(
                store.apply_xml_import(&plan).unwrap().credit_applications,
                1
            );
            reconcile(&store, "2026-01-14", 10500, 0);
        }
    }

    #[test]
    fn voucher_keys_survive_import_and_block_duplicate_records() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Voucher keys").unwrap();
        store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Capital", Category::Equity)
            .unwrap();
        let key = "ad4056cb-9b5b-4f58-9fa6-f38a2116eb99-00142cfa:00000017";
        let entry = |reference, key, child| {
            format!(
                r#"
            <VOUCHER VCHKEY="{key}" VCHTYPE="Journal" ACTION="Create" OBJVIEW="Accounting Voucher View">
             <DATE>20260101</DATE><VOUCHERTYPENAME>Journal</VOUCHERTYPENAME><VOUCHERNUMBER>{reference}</VOUCHERNUMBER>{child}
             <ALLLEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>-73.25</AMOUNT></ALLLEDGERENTRIES.LIST>
             <ALLLEDGERENTRIES.LIST><LEDGERNAME>Capital</LEDGERNAME><AMOUNT>73.25</AMOUNT></ALLLEDGERENTRIES.LIST>
            </VOUCHER>"#
            )
        };
        let envelope = |body: &str| {
            format!("<ENVELOPE><BODY><DATA><MESSAGE>{body}</MESSAGE></DATA></BODY></ENVELOPE>")
        };
        let first = entry("J-01", key, "");
        let upper_key = key.to_ascii_uppercase();
        let second = entry("J-02", upper_key.as_str(), "");
        let conflicts = entry("J-01", key, "<VCHKEY>different-key</VCHKEY>");
        let conflict_plan = store
            .preview_xml_import(company, envelope(&conflicts).as_bytes())
            .unwrap();
        assert!(conflict_plan
            .issues()
            .iter()
            .any(|issue| issue.contains("conflicting VCHKEY values")));
        let batch_plan = store
            .preview_xml_import(company, envelope(&format!("{first}{second}")).as_bytes())
            .unwrap();
        assert!(batch_plan
            .issues()
            .iter()
            .any(|issue| issue.contains("imported identifier voucher key already exists")));
        let stale = store
            .preview_xml_import(company, envelope(&second).as_bytes())
            .unwrap();
        assert!(stale.can_apply(), "{:?}", stale.issues());
        let plan = store
            .preview_xml_import(company, envelope(&first).as_bytes())
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert_eq!(store.apply_xml_import(&plan).unwrap().vouchers, 1);
        assert!(store
            .apply_xml_import(&stale)
            .unwrap_err()
            .to_string()
            .contains("identifier already exists"));
        let duplicate = store
            .preview_xml_import(company, envelope(&second).as_bytes())
            .unwrap();
        assert!(duplicate
            .issues()
            .iter()
            .any(|issue| issue.contains("imported identifier voucher key already exists")));
        assert_eq!(store.vouchers(company).unwrap().len(), 1);
        let stored: String = store.conn.query_row("SELECT voucher_key FROM imported_record_metadata WHERE company_id = ?1 AND kind = 'voucher'", [company], |row| row.get(0)).unwrap();
        assert_eq!(stored, key);
    }

    #[test]
    fn financial_notes_keep_one_voucher_across_split_and_opening_bills() {
        for (kind, document, note_kind, settlement_kind, category, trade_category, direction) in [
            (
                InvoiceKind::Sale,
                "Sales",
                "Credit Note",
                "Receipt",
                Category::Asset,
                Category::Income,
                1_i64,
            ),
            (
                InvoiceKind::Purchase,
                "Purchase",
                "Debit Note",
                "Payment",
                Category::Liability,
                Category::Expense,
                -1_i64,
            ),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Note import").unwrap();
            let party = store.create_ledger(company, "Party", category).unwrap();
            store
                .create_ledger(company, "Trade", trade_category)
                .unwrap();
            let bank = store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            let adjustment = store
                .create_ledger(company, "Price allowance", Category::Expense)
                .unwrap();
            let opening = store
                .post_opening_bill(
                    company,
                    &OpeningBillDraft {
                        kind,
                        party_ledger_id: party,
                        reference: "OPEN".into(),
                        original_date: "2025-12-20".into(),
                        opening_date: "2026-01-01".into(),
                        due_date: None,
                        credit_days: Some(30),
                        amount: 5000,
                    },
                )
                .unwrap();
            let debit = |amount| amount * direction;
            let bill = |reference, mode, amount| {
                format!(
                "<BILLALLOCATIONS.LIST><NAME>{reference}</NAME><BILLTYPE>{mode}</BILLTYPE><AMOUNT>{amount}</AMOUNT></BILLALLOCATIONS.LIST>"
            )
            };
            let ledger = |name, amount, details| {
                format!(
                "<LEDGERENTRIES.LIST><LEDGERNAME>{name}</LEDGERNAME><AMOUNT>{amount}</AMOUNT>{details}</LEDGERENTRIES.LIST>"
            )
            };
            let voucher = |date, kind, reference, header, entries| {
                format!(
                "<VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME><VOUCHERNUMBER>{reference}</VOUCHERNUMBER>{header}{entries}</VOUCHER>"
            )
            };
            let receipt = voucher(
                "20260104",
                settlement_kind,
                "SET-1",
                "",
                ledger(
                    "Party",
                    debit(15),
                    bill("A", "Agst Ref", debit(10)) + &bill("OPEN", "Agst Ref", debit(5)),
                ) + &ledger("Bank", debit(-15), String::new()),
            );
            let invoice = voucher(
                "20260102",
                document,
                "INV-1",
                "<ISINVOICE>Yes</ISINVOICE><PARTYLEDGERNAME>Party</PARTYLEDGERNAME>",
                ledger(
                    "Party",
                    debit(-100),
                    bill("A", "New Ref", debit(-40)) + &bill("B", "New Ref", debit(-60)),
                ) + &ledger("Trade", debit(100), String::new()),
            );
            let note = voucher(
                "20260103",
                note_kind,
                "NOTE-1",
                "<ISINVOICE>Yes</ISINVOICE><PERSISTEDVIEW>Invoice Voucher View</PERSISTEDVIEW>",
                ledger(
                    "Party",
                    debit(55),
                    bill("A", "Agst Ref", debit(15))
                        + &bill("B", "Agst Ref", debit(20))
                        + &bill("OPEN", "Agst Ref", debit(20)),
                ) + &ledger("Price allowance", debit(-55), String::new()),
            );
            let envelope = |notes: &str| {
                format!("<ENVELOPE><BODY><DATA><MESSAGE>{receipt}{notes}{invoice}</MESSAGE></DATA></BODY></ENVELOPE>")
            };
            for (invalid, issue) in [
                (
                    note.replacen("Agst Ref", "On Account", 1),
                    "note needs an existing bill or a new named credit reference",
                ),
                (
                    note.replace("<NAME>B</NAME>", "<NAME>A</NAME>"),
                    "bill reference occurs more than once",
                ),
                (
                    note.replace("<NAME>B</NAME>", "<NAME>MISSING</NAME>"),
                    "referenced bill MISSING is unavailable",
                ),
                (
                    note.replace(
                        &format!("<AMOUNT>{}</AMOUNT>", debit(55)),
                        &format!("<AMOUNT>{}</AMOUNT>", debit(56)),
                    ),
                    "do not equal the party amount",
                ),
            ] {
                let plan = store
                    .preview_xml_import(company, envelope(&invalid).as_bytes())
                    .unwrap();
                assert!(
                    plan.issues().iter().any(|found| found.contains(issue)),
                    "{:?}",
                    plan.issues()
                );
                assert!(store.apply_xml_import(&plan).is_err());
            }
            assert_eq!(store.vouchers(company).unwrap().len(), 1);
            let plan = store
                .preview_xml_import(company, envelope(&note).as_bytes())
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let stale_note = voucher(
                "20260105",
                note_kind,
                "NOTE-2",
                "",
                ledger("Party", debit(20), bill("OPEN", "Agst Ref", debit(20)))
                    + &ledger("Price allowance", debit(-20), String::new()),
            );
            let stale_xml = format!(
                "<ENVELOPE><BODY><DATA><MESSAGE>{stale_note}</MESSAGE></DATA></BODY></ENVELOPE>"
            );
            let stale = store
                .preview_xml_import(company, stale_xml.as_bytes())
                .unwrap();
            assert!(stale.can_apply(), "{:?}", stale.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (counts.invoices, counts.bill_notes, counts.settlements),
                (1, 1, 1)
            );
            assert_eq!(store.vouchers(company).unwrap().len(), 4);
            let outstanding = |store: &Store, date| {
                store
                    .outstanding_invoices(company, date)
                    .unwrap()
                    .into_iter()
                    .map(|row| (row.bill_reference, row.outstanding))
                    .collect::<BTreeMap<_, _>>()
            };
            let balance = |store: &Store, ledger, date| {
                store
                    .ledger_register(company, ledger, None, Some(date))
                    .unwrap()
                    .closing_balance
            };
            for (date, expected, bank_balance, note_balance) in [
                ("2026-01-02", [4000, 6000, 5000], 0, 0),
                ("2026-01-03", [2500, 4000, 3000], 0, 5500),
                ("2026-01-04", [1500, 4000, 2500], 1500, 5500),
            ] {
                let bills = outstanding(&store, date);
                assert_eq!([bills["A"], bills["B"], bills["OPEN"]], expected);
                assert_eq!(
                    balance(&store, party, date),
                    expected.iter().sum::<i128>() * i128::from(direction)
                );
                assert_eq!(
                    balance(&store, bank, date),
                    bank_balance * i128::from(direction)
                );
                assert_eq!(
                    balance(&store, adjustment, date),
                    note_balance * i128::from(direction)
                );
            }
            let saved = &store.bill_note_allocations(company, opening).unwrap()[0];
            assert_eq!(
                (&*saved.reference, &*saved.bill_reference, saved.amount),
                ("NOTE-1", "OPEN", 2000)
            );
            let note_id = saved.voucher_id;
            let original = store
                .vouchers(company)
                .unwrap()
                .into_iter()
                .find(|v| v.id == note_id)
                .unwrap();
            assert_eq!(original.party_lines, [0]);
            assert!(store
                .update_voucher(
                    company,
                    note_id,
                    &original.date,
                    "Changed",
                    &original.postings
                )
                .is_err());
            assert!(store.apply_xml_import(&plan).is_err());
            assert!(store
                .apply_xml_import(&stale)
                .unwrap_err()
                .to_string()
                .contains("Bill OPEN changed"));
            assert_eq!(store.vouchers(company).unwrap().len(), 4);
            store
                .reverse_voucher(company, note_id, "2026-01-06", "NOTE-REV", "Undo note")
                .unwrap();
            let bills = outstanding(&store, "2026-01-06");
            assert_eq!([bills["A"], bills["B"], bills["OPEN"]], [3000, 6000, 4500]);
            let before_reversal = outstanding(&store, "2026-01-04");
            assert_eq!(
                [
                    before_reversal["A"],
                    before_reversal["B"],
                    before_reversal["OPEN"]
                ],
                [1500, 4000, 2500]
            );
            assert_eq!(balance(&store, bank, "2026-01-06"), i128::from(debit(1500)));
            assert_eq!(balance(&store, adjustment, "2026-01-06"), 0);
            assert_eq!(
                balance(&store, party, "2026-01-06"),
                i128::from(debit(13500))
            );
            assert_eq!(
                store.bill_note_allocations(company, opening).unwrap()[0]
                    .reversal_date
                    .as_deref(),
                Some("2026-01-06")
            );
        }
    }

    #[test]
    fn opening_advances_fund_later_invoices_without_cash_movement() {
        for (kind, group, side, source_type) in [
            ("Sales", "Sundry Debtors", -1_i64, "Advance"),
            ("Purchase", "Sundry Creditors", 1_i64, "New Ref"),
        ] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Opening advance migration").unwrap();
            let bank = store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            store
                .create_ledger(
                    company,
                    "Trade",
                    if side < 0 {
                        Category::Income
                    } else {
                        Category::Expense
                    },
                )
                .unwrap();
            let masters = format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
                <LEDGER NAME="Party"><PARENT>{group}</PARENT><ISBILLWISEON>Yes</ISBILLWISEON>
                  <OPENINGBALANCE>{}</OPENINGBALANCE>
                  <BILLALLOCATIONS.LIST><NAME>OLD-BILL</NAME><BILLDATE>20251215</BILLDATE>
                    <OPENINGBALANCE>{}</OPENINGBALANCE>
                  </BILLALLOCATIONS.LIST>
                  <BILLALLOCATIONS.LIST><NAME>DEPOSIT</NAME><BILLDATE>20251201</BILLDATE>
                    <ISADVANCE>Yes</ISADVANCE><BILLTYPE>{source_type}</BILLTYPE><OPENINGBALANCE>{}</OPENINGBALANCE>
                    <BILLCREDITPERIOD>30 Days</BILLCREDITPERIOD>
                  </BILLALLOCATIONS.LIST>
                </LEDGER>
            </MESSAGE></DATA></BODY></ENVELOPE>"#,
                side * 70,
                side * 90,
                -side * 50
            );
            let invoice = format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE><VOUCHER>
                <DATE>20260105</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME>
                <VOUCHERNUMBER>INV-NEW</VOUCHERNUMBER><ISINVOICE>Yes</ISINVOICE>
                <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><ISPARTYLEDGER>Yes</ISPARTYLEDGER>
                  <AMOUNT>{}</AMOUNT>
                  <BILLALLOCATIONS.LIST><NAME>DEPOSIT</NAME><BILLTYPE>Agst Ref</BILLTYPE>
                    <AMOUNT>{}</AMOUNT><BILLCREDITPERIOD>31-Dec-2025</BILLCREDITPERIOD>
                  </BILLALLOCATIONS.LIST>
                  <BILLALLOCATIONS.LIST><NAME>REMAINDER</NAME><BILLTYPE>New Ref</BILLTYPE><AMOUNT>{}</AMOUNT></BILLALLOCATIONS.LIST>
                </LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>{}</AMOUNT></LEDGERENTRIES.LIST>
            </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#,
                side * 45,
                side * 30,
                side * 15,
                -side * 45
            );
            let opening = Some("2026-01-01");
            let blocked = store
                .preview_xml_import_batch(
                    company,
                    &[
                        masters.as_bytes(),
                        invoice.replace("20260105", "20251231").as_bytes(),
                    ],
                    opening,
                )
                .unwrap();
            assert!(
                blocked
                    .issues()
                    .iter()
                    .any(|issue| issue.contains("precedes advance")),
                "{:?}",
                blocked.issues()
            );
            assert!(store.apply_xml_import(&blocked).is_err());
            assert!(store.vouchers(company).unwrap().is_empty());

            if side > 0 {
                let plan = store
                    .preview_xml_import_with_opening_date(company, masters.as_bytes(), opening)
                    .unwrap();
                assert!(plan.can_apply(), "{:?}", plan.issues());
                let counts = store.apply_xml_import(&plan).unwrap();
                assert_eq!(
                    (
                        counts.opening_bills,
                        counts.opening_advances,
                        counts.opening_on_account
                    ),
                    (1, 1, 1)
                );
            }
            let plan = if side > 0 {
                store.preview_xml_import(company, invoice.as_bytes())
            } else {
                store.preview_xml_import_batch(
                    company,
                    &[masters.as_bytes(), invoice.as_bytes()],
                    opening,
                )
            }
            .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            if side < 0 {
                assert_eq!(
                    (
                        counts.opening_bills,
                        counts.opening_advances,
                        counts.opening_on_account
                    ),
                    (1, 1, 1)
                );
            }
            assert_eq!(counts.advance_applications, 1);
            let advance = store.party_advances(company, None).unwrap().remove(0);
            assert_eq!(advance.opening_original_date.as_deref(), Some("2025-12-01"));
            assert_eq!(advance.source_bill.as_ref().unwrap().kind, source_type);
            assert_eq!((advance.applied, advance.available), (3000, 2000));
            assert!(store
                .party_advances(company, Some("2025-12-31"))
                .unwrap()
                .is_empty());
            let party = advance.party_ledger_id;
            let sign = -i128::from(side);
            let balance = |s: &Store, ledger| {
                s.ledger_register(company, ledger, None, None)
                    .unwrap()
                    .closing_balance
            };
            assert_eq!(balance(&store, party), sign * 11500);
            assert_eq!(balance(&store, bank), 0);
            let party_report = store
                .party_outstanding(company, "2026-01-05")
                .unwrap()
                .remove(0);
            assert_eq!(party_report.opening_on_account, sign * 3000);
            assert_eq!(party_report.receivable + party_report.payable, 10500);
            let original = store
                .vouchers(company)
                .unwrap()
                .into_iter()
                .find(|voucher| voucher.id == advance.voucher_id)
                .unwrap();
            assert_eq!(original.kind, VoucherKind::Journal);
            assert!(store
                .reverse_party_advance(company, advance.id, "2026-01-10", "REV", "Correction")
                .is_err());
            let allocation = store
                .advance_allocations(company, advance.id, None)
                .unwrap()
                .remove(0);
            store
                .release_advance_allocation(company, allocation.id, "2026-01-07", "Correction")
                .unwrap();
            let reversal = store
                .reverse_party_advance(company, advance.id, "2026-01-10", "REV", "Correction")
                .unwrap();
            let reverse_voucher = store
                .vouchers(company)
                .unwrap()
                .into_iter()
                .find(|voucher| voucher.id == reversal)
                .unwrap();
            assert_eq!(reverse_voucher.kind, VoucherKind::Journal);
            assert_eq!(balance(&store, party), sign * 16500);
            assert_eq!(balance(&store, bank), 0);
            assert_eq!(
                store.party_advances(company, Some("2026-01-09")).unwrap()[0].available,
                5000
            );
            assert_eq!(
                store.party_advances(company, Some("2026-01-10")).unwrap()[0].available,
                0
            );
        }
    }

    #[test]
    fn opening_import_keeps_signed_on_account_amounts_and_named_bills() {
        for (group, side) in [("Sundry Debtors", 1_i64), ("Sundry Creditors", -1)] {
            for net in [150, 60, 0, -25] {
                let mut store = Store::memory().unwrap();
                let company = store.create_company("Opening balance migration").unwrap();
                let bank = store
                    .create_ledger(company, "Bank", Category::Asset)
                    .unwrap();
                let xml = format!(
                    r#"<ENVELOPE><BODY><DATA><MESSAGE>
                    <LEDGER NAME="Party"><PARENT>{group}</PARENT><ISBILLWISEON>Yes</ISBILLWISEON>
                      <OPENINGBALANCE>{}</OPENINGBALANCE>
                      <BILLALLOCATIONS.LIST><NAME>OLD-BILL</NAME><BILLDATE>20251215</BILLDATE>
                        <ISADVANCE>No</ISADVANCE><OPENINGBALANCE>{}</OPENINGBALANCE>
                        <BILLCREDITPERIOD>15 Days</BILLCREDITPERIOD>
                      </BILLALLOCATIONS.LIST>
                    </LEDGER>
                </MESSAGE></DATA></BODY></ENVELOPE>"#,
                    -side * net,
                    -side * 100
                );
                let opening = "2026-01-01";
                let plan = store
                    .preview_xml_import_with_opening_date(company, xml.as_bytes(), Some(opening))
                    .unwrap();
                assert!(plan.can_apply(), "{:?}", plan.issues());
                assert_eq!(
                    (
                        plan.opening_count(),
                        plan.opening_bill_count(),
                        plan.opening_on_account_count()
                    ),
                    (0, 1, 1)
                );
                let counts = store.apply_xml_import(&plan).unwrap();
                assert_eq!((counts.opening_bills, counts.opening_on_account), (1, 1));
                let bill = &store.outstanding_invoices(company, opening).unwrap()[0];
                let party = bill.party_ledger_id;
                assert_eq!(bill.outstanding, 10000);
                assert_eq!(bill.due_date.as_deref(), Some("2025-12-30"));
                let residual = side * (net - 100) * 100;
                let balance = store.opening_balance(company, party).unwrap().unwrap();
                assert_eq!((balance.amount, balance.on_account), (residual, true));
                assert!(store
                    .set_opening_balance(company, party, opening, 1000)
                    .is_err());
                assert!(store
                    .set_opening_on_account_balance(company, party, "2026-01-02", 1000)
                    .is_err());
                let ledger_at = |s: &Store, date| {
                    s.ledger_register(company, party, None, Some(date))
                        .unwrap()
                        .closing_balance
                };
                assert_eq!(ledger_at(&store, opening), i128::from(side * net * 100));
                assert!(store
                    .party_outstanding(company, "2025-12-31")
                    .unwrap()
                    .is_empty());

                let target = BillSettlementTarget {
                    invoice_id: bill.invoice_id,
                    split_id: bill.split_id,
                };
                store
                    .settle_bills(
                        company,
                        bank,
                        "2026-01-02",
                        "SETTLE",
                        &[BillSettlementAllocation {
                            target,
                            amount: 1500,
                        }],
                    )
                    .unwrap();
                let totals_at =
                    |s: &Store, date| s.party_outstanding(company, date).unwrap().remove(0);
                let before = totals_at(&store, "2026-01-03");
                assert_eq!(before.opening_on_account, i128::from(residual));
                assert_eq!(before.receivable + before.payable, 8500);
                assert_eq!(
                    before.receivable - before.payable + before.opening_on_account,
                    ledger_at(&store, "2026-01-03")
                );

                store
                    .clear_opening_balance(
                        company,
                        party,
                        "2026-01-04",
                        "CLEAR",
                        "Clear on account",
                    )
                    .unwrap();
                assert!(store.opening_clear_history(company, party).unwrap()[0].on_account);
                assert_eq!(
                    totals_at(&store, "2026-01-03").opening_on_account,
                    i128::from(residual)
                );
                let after = totals_at(&store, "2026-01-04");
                assert_eq!(after.opening_on_account, 0);
                assert_eq!(after.receivable + after.payable, 8500);
                assert_eq!(
                    after.receivable - after.payable,
                    ledger_at(&store, "2026-01-04")
                );
            }
        }
    }

    #[test]
    fn opening_bill_and_settlement_terms_use_the_original_bill_date() {
        let masters = r#"<ENVELOPE><BODY><DATA><MESSAGE>
          <LEDGER NAME="Customer"><PARENT>Sundry Debtors</PARENT><ISBILLWISEON>Yes</ISBILLWISEON>
            <OPENINGBALANCE>-100.00</OPENINGBALANCE>
            <BILLALLOCATIONS.LIST><NAME>DEC-01</NAME><BILLDATE>20251201</BILLDATE>
              <BILLTYPE>New Ref</BILLTYPE><AMOUNT>-40.00</AMOUNT>
              <BILLCREDITPERIOD>31-Jan-2026</BILLCREDITPERIOD>
            </BILLALLOCATIONS.LIST>
            <BILLALLOCATIONS.LIST><NAME>DEC-10</NAME><BILLDATE>20251210</BILLDATE>
              <BILLTYPE>New Ref</BILLTYPE><AMOUNT>-60.00</AMOUNT>
              <BILLCREDITPERIOD>30 Days</BILLCREDITPERIOD>
            </BILLALLOCATIONS.LIST>
          </LEDGER>
        </MESSAGE></DATA></BODY></ENVELOPE>"#;
        let receipt = r#"<ENVELOPE><BODY><DATA><MESSAGE><VOUCHER>
          <DATE>20260105</DATE><VOUCHERTYPENAME>Receipt</VOUCHERTYPENAME><VOUCHERNUMBER>RC-OPEN</VOUCHERNUMBER>
          <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>35.00</AMOUNT>
            <BILLALLOCATIONS.LIST><NAME>DEC-01</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>15.00</AMOUNT>
              <BILLCREDITPERIOD>61 Days</BILLCREDITPERIOD>
            </BILLALLOCATIONS.LIST>
            <BILLALLOCATIONS.LIST><NAME>DEC-10</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>20.00</AMOUNT>
              <BILLCREDITPERIOD>20260109</BILLCREDITPERIOD>
            </BILLALLOCATIONS.LIST>
          </LEDGERENTRIES.LIST>
          <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>-35.00</AMOUNT></LEDGERENTRIES.LIST>
        </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#;
        let opening = Some("2026-01-01");
        for separate in [false, true] {
            let mut store = Store::memory().unwrap();
            let company = store.create_company("Opening bill migration").unwrap();
            let bank = store
                .create_ledger(company, "Bank", Category::Asset)
                .unwrap();
            let wrong_due = masters.replace("31-Jan-2026", "30-Nov-2025");
            let invalid = store
                .preview_xml_import_with_opening_date(company, wrong_due.as_bytes(), opening)
                .unwrap();
            assert!(
                invalid
                    .issues()
                    .iter()
                    .any(|issue| issue.contains("due date precedes")),
                "{:?}",
                invalid.issues()
            );
            assert!(store.apply_xml_import(&invalid).is_err());

            if separate {
                let plan = store
                    .preview_xml_import_with_opening_date(company, masters.as_bytes(), opening)
                    .unwrap();
                assert!(plan.can_apply(), "{:?}", plan.issues());
                assert_eq!(store.apply_xml_import(&plan).unwrap().opening_bills, 2);
            }
            let before_receipt = store.vouchers(company).unwrap().len();
            let preview = |store: &Store, transactions: &str| {
                if separate {
                    store.preview_xml_import(company, transactions.as_bytes())
                } else {
                    store.preview_xml_import_batch(
                        company,
                        &[masters.as_bytes(), transactions.as_bytes()],
                        opening,
                    )
                }
            };
            let conflict = preview(&store, &receipt.replace("61 Days", "62 Days")).unwrap();
            assert!(
                conflict
                    .issues()
                    .iter()
                    .any(|issue| issue.contains("credit terms differ from the saved bill DEC-01")),
                "{:?}",
                conflict.issues()
            );
            assert!(store.apply_xml_import(&conflict).is_err());
            assert_eq!(store.vouchers(company).unwrap().len(), before_receipt);

            let plan = preview(&store, receipt).unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            let counts = store.apply_xml_import(&plan).unwrap();
            assert_eq!(
                (counts.opening_bills, counts.settlements),
                (if separate { 0 } else { 2 }, 1)
            );
            let rows = store.outstanding_invoices(company, "2026-02-01").unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| (
                        row.bill_reference.as_str(),
                        row.due_date.as_deref(),
                        row.outstanding,
                        row.overdue_days
                    ))
                    .collect::<Vec<_>>(),
                [
                    ("DEC-01", Some("2026-01-31"), 2500, Some(1)),
                    ("DEC-10", Some("2026-01-09"), 4000, Some(23))
                ]
            );
            let closing = |ledger| {
                store
                    .ledger_register(company, ledger, None, None)
                    .unwrap()
                    .closing_balance
            };
            assert_eq!(closing(rows[0].party_ledger_id), 6500);
            assert_eq!(closing(bank), 3500);
            let before = store.outstanding_invoices(company, "2026-01-04").unwrap();
            assert_eq!(
                before.iter().map(|row| row.outstanding).sum::<i128>(),
                10000
            );
        }
    }

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
            let with_terms = |xml: String, term: &str| {
                xml.replacen(
                    "</BILLALLOCATIONS.LIST>",
                    &format!("<BILLCREDITPERIOD>{term}</BILLCREDITPERIOD></BILLALLOCATIONS.LIST>"),
                    1,
                )
            };
            let source_term = if kind == "Sales" {
                "15-Feb-2026"
            } else {
                "45 Days"
            };
            let advance = with_terms(
                cash_voucher("20260101", "CASH-ADV", "DEPOSIT", "New Ref", 80),
                source_term,
            );
            let first = invoice(
                "20260110",
                "INV-1",
                100,
                with_terms(allocation("DEPOSIT", "Agst Ref", 50), "20260215")
                    + &allocation("OPEN", "New Ref", 50),
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
                    transactions.replace("20260215", "20260216"),
                    "Credit terms differ",
                ),
                (
                    transactions.replace(
                        &advance,
                        &cash_voucher("20260101", "CASH-ADV", "DEPOSIT", "On Account", 80),
                    ),
                    "unavailable",
                ),
                (
                    transactions.replace(
                        &advance,
                        &with_terms(
                            cash_voucher("20260101", "CASH-ADV", "DEPOSIT", "On Account", 80),
                            source_term,
                        ),
                    ),
                    "on-account allocations cannot carry credit terms",
                ),
                (
                    transactions.replace(
                        &advance,
                        &cash_voucher("20260101", "CASH-ADV", "", "New Ref", 80),
                    ),
                    "needs a bill reference",
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
            let source = advance.source_bill.as_ref().unwrap();
            assert_eq!(
                (source.kind.as_str(), source.reference.as_deref()),
                ("New Ref", Some("DEPOSIT"))
            );
            let expected_terms = if kind == "Sales" {
                (Some("2026-02-15"), None)
            } else {
                (None, Some(45))
            };
            assert_eq!(
                (source.due_date.as_deref(), source.credit_days),
                expected_terms
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
    fn native_and_imported_supplier_discounts_post_the_same_amounts() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Supplier settlement").unwrap();
        let supplier = store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let bank = store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        let discount = store
            .create_ledger(company, "Discount received", Category::Income)
            .unwrap();
        store
            .create_ledger(company, "Purchases", Category::Expense)
            .unwrap();
        let plan = store.preview_xml_import(company, br#"<ENVELOPE><BODY><DATA><MESSAGE><VOUCHER>
            <DATE>20260101</DATE><VOUCHERTYPENAME>Purchase</VOUCHERTYPENAME><VOUCHERNUMBER>INV-1</VOUCHERNUMBER>
            <ISINVOICE>Yes</ISINVOICE>
            <LEDGERENTRIES.LIST><LEDGERNAME>Supplier</LEDGERNAME><ISPARTYLEDGER>Yes</ISPARTYLEDGER><AMOUNT>100</AMOUNT>
                <BILLALLOCATIONS.LIST><NAME>BILL-1</NAME><BILLTYPE>New Ref</BILLTYPE><AMOUNT>100</AMOUNT></BILLALLOCATIONS.LIST>
            </LEDGERENTRIES.LIST>
            <LEDGERENTRIES.LIST><LEDGERNAME>Purchases</LEDGERNAME><AMOUNT>-100</AMOUNT></LEDGERENTRIES.LIST>
        </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let bill = &store.outstanding_invoices(company, "2026-01-01").unwrap()[0];
        let allocations = [BillSettlementAllocation {
            target: BillSettlementTarget {
                invoice_id: bill.invoice_id,
                split_id: bill.split_id,
            },
            amount: 6000,
        }];
        let rejected = [SettlementDiscount {
            ledger_id: discount,
            amount: 6000,
        }];
        assert!(store
            .settle_bills_with_discounts(
                company,
                bank,
                "2026-01-02",
                "P-1",
                &allocations,
                &[],
                &rejected
            )
            .is_err());
        assert_eq!(store.vouchers(company).unwrap().len(), 1);
        let discounts = [SettlementDiscount {
            ledger_id: discount,
            amount: 500,
        }];
        let (voucher, _) = store
            .settle_bills_with_discounts(
                company,
                bank,
                "2026-01-02",
                "P-1",
                &allocations,
                &[],
                &discounts,
            )
            .unwrap();
        let check = |store: &Store, date| {
            let balance = |ledger| {
                store
                    .ledger_register(company, ledger, None, Some(date))
                    .unwrap()
                    .closing_balance
            };
            assert_eq!(
                (balance(supplier), balance(bank), balance(discount)),
                (-4000, -5500, -500)
            );
            let bills = store.outstanding_invoices(company, date).unwrap();
            assert_eq!(bills[0].outstanding, 4000);
        };
        check(&store, "2026-01-02");
        store
            .reverse_settlement(
                company,
                voucher,
                "2026-01-03",
                "P-1-R",
                "Reverse test payment",
            )
            .unwrap();
        let plan = store.preview_xml_import(company, br#"<ENVELOPE><BODY><DATA><MESSAGE><VOUCHER>
            <DATE>20260104</DATE><VOUCHERTYPENAME>Payment</VOUCHERTYPENAME><VOUCHERNUMBER>P-2</VOUCHERNUMBER>
            <LEDGERENTRIES.LIST><LEDGERNAME>Supplier</LEDGERNAME><AMOUNT>-60</AMOUNT>
                <BILLALLOCATIONS.LIST><NAME>BILL-1</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>-60</AMOUNT></BILLALLOCATIONS.LIST>
            </LEDGERENTRIES.LIST>
            <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>55</AMOUNT></LEDGERENTRIES.LIST>
            <LEDGERENTRIES.LIST><LEDGERNAME>Discount received</LEDGERNAME><AMOUNT>5</AMOUNT></LEDGERENTRIES.LIST>
        </VOUCHER></MESSAGE></DATA></BODY></ENVELOPE>"#).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert_eq!(store.apply_xml_import(&plan).unwrap().settlements, 1);
        check(&store, "2026-01-04");
    }

    #[test]
    fn discounted_receipt_with_advances_reconciles_and_reverses_as_one_voucher() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Migration").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        let bank = store
            .create_ledger(company, "Bank", Category::Asset)
            .unwrap();
        let discount = store
            .create_ledger(company, "Discount allowed", Category::Expense)
            .unwrap();
        let allowance = store
            .create_ledger(company, "Early payment allowance", Category::Income)
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
        let deposit = bill("RC-DEPOSIT", "Advance", 20) + &bill("HOLD", "New Ref", 10);
        let xml = |deposit: &str| {
            format!(
                r#"<ENVELOPE><BODY><DATA><MESSAGE>
            {later}<VOUCHER>
                <DATE>20260110</DATE><VOUCHERTYPENAME>Receipt</VOUCHERTYPENAME>
                <VOUCHERNUMBER>RC-01</VOUCHERNUMBER><GUID>mixed-receipt-example</GUID>
                <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>90</AMOUNT>{allocations}{deposit}</LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Discount allowed</LEDGERNAME><AMOUNT>-3</AMOUNT></LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Bank</LEDGERNAME><AMOUNT>-85</AMOUNT></LEDGERENTRIES.LIST>
                <LEDGERENTRIES.LIST><LEDGERNAME>Early payment allowance</LEDGERNAME><AMOUNT>-2</AMOUNT></LEDGERENTRIES.LIST>
            </VOUCHER>{first}</MESSAGE></DATA></BODY></ENVELOPE>"#
            )
        };
        for (invalid, expected) in [
            (
                xml(&bill("RC-DEPOSIT", "Advance", 29)),
                "do not equal the party",
            ),
            (
                xml(&(bill("RC-DEPOSIT", "Advance", 20) + &bill("RC-DEPOSIT", "New Ref", 10))),
                "named advance reference occurs more than once",
            ),
            (
                xml(&deposit).replace("<AMOUNT>-3</AMOUNT>", "<AMOUNT>3</AMOUNT>"),
                "discount direction",
            ),
            (
                xml(&deposit).replace("<AMOUNT>-3</AMOUNT>", "<AMOUNT>-4</AMOUNT>"),
                "amounts or directions are inconsistent",
            ),
            (
                xml(&deposit).replace("<AMOUNT>-3</AMOUNT>", "<AMOUNT>-70</AMOUNT>"),
                "discounts exceed",
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
            (2, 1, 2, 1)
        );
        assert_eq!(store.vouchers(company).unwrap().len(), 3);
        let ledger = |s: &Store, ledger| {
            s.ledger_register(company, ledger, None, None)
                .unwrap()
                .closing_balance
        };
        assert_eq!(ledger(&store, bank), 8500);
        assert_eq!(ledger(&store, customer), 3000);
        assert_eq!(ledger(&store, discount), 300);
        assert_eq!(ledger(&store, allowance), 200);
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
        let advances = store.party_advances(company, None).unwrap();
        let find = |name: &str| {
            advances
                .iter()
                .find(|advance| {
                    advance
                        .source_bill
                        .as_ref()
                        .and_then(AdvanceSourceBill::named_reference)
                        == Some(name)
                })
                .unwrap()
        };
        let advance = find("RC-DEPOSIT");
        let held = find("HOLD");
        assert!(advances
            .iter()
            .all(|advance| advance.shares_voucher && advance.includes_settlements));
        assert_eq!(settlement.voucher_id, advance.voucher_id);
        assert_eq!(settlement.voucher_id, held.voucher_id);
        assert_eq!((held.amount, held.available), (1000, 1000));
        use crate::cash_flow::{CashClass, CashRole};
        store
            .save_cash_ledger_rule(company, bank, CashRole::Cash, "Test bank classification")
            .unwrap();
        store
            .save_cash_ledger_rule(
                company,
                customer,
                CashRole::Counterpart(CashClass::Operating),
                "Test customer classification",
            )
            .unwrap();
        assert_eq!(
            (settlement.amount, advance.amount, advance.available),
            (6000, 2000, 0)
        );
        assert_eq!(
            (settlement.voucher_cash, settlement.voucher_discount),
            (8500, 500)
        );
        assert!(advance.includes_settlements);
        let partial_reversal = store.conn.execute(
            "UPDATE advance_vouchers SET reversal_voucher_id = ?1, reversal_reason = 'Test partial reversal' WHERE voucher_id = ?2",
            params![settlement.voucher_id, advance.voucher_id],
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
            .reverse_party_advance(company, held.id, "2026-01-20", "REV", "Test reversal")
            .is_err());
        assert_eq!(ledger(&store, bank), 8500);
        let extra = store
            .apply_party_advance_to_bill(
                company,
                held.id,
                BillSettlementTarget {
                    invoice_id: rows[0].invoice_id,
                    split_id: rows[0].split_id,
                },
                "2026-01-16",
                400,
                "Apply separately",
            )
            .unwrap();
        let history = store
            .advance_allocations(company, advance.id, None)
            .unwrap();
        store
            .release_advance_allocation(company, history[0].id, "2026-01-18", "Test release")
            .unwrap();
        assert!(store
            .reverse_party_advance(company, advance.id, "2026-01-20", "REV", "Test reversal")
            .is_err());
        store
            .release_advance_allocation(company, extra, "2026-01-19", "Test release")
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
        assert_eq!(ledger(&store, discount), 0);
        assert_eq!(ledger(&store, allowance), 0);
        let cash = store.cash_flow_report(company, None, None).unwrap();
        assert_eq!(cash.entries.len(), 2);
        let operating = cash.totals[&CashClass::Operating];
        assert_eq!(
            (cash.closing, operating.inflows, operating.outflows),
            (0, 8500, 8500)
        );
        assert_eq!(
            store
                .invoice_settlements(company, rows[0].invoice_id)
                .unwrap()[0]
                .reversal_voucher_id,
            Some(reversed)
        );
        let reversed_advances = store.party_advances(company, None).unwrap();
        assert_eq!(reversed_advances.len(), 2);
        assert!(reversed_advances.iter().all(|row| row.available == 0
            && row.reversal.as_ref().map(|r| r.voucher_id) == Some(reversed)));
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
        use crate::advances::AdvancePartDraft;
        let parts = [
            AdvancePartDraft {
                reference: "PHASE-ONE".into(),
                amount: 200,
            },
            AdvancePartDraft {
                reference: "PHASE-TWO".into(),
                amount: 300,
            },
        ];
        let (native, _) = store
            .settle_bills_with_advance_parts(
                company,
                bank,
                "2026-01-21",
                "MANUAL",
                &[BillSettlementAllocation {
                    target,
                    amount: 1500,
                }],
                &parts,
            )
            .unwrap();
        assert_eq!(ledger(&store, bank), 2000);
        let native_parts = store.party_advances(company, None).unwrap();
        assert_eq!(
            native_parts
                .iter()
                .filter(|row| row.voucher_id == native)
                .map(|row| row.available)
                .sum::<i128>(),
            500
        );
        store
            .reverse_settlement(company, native, "2026-01-22", "MANUAL-REV", "Test reversal")
            .unwrap();
        assert_eq!(ledger(&store, bank), 0);
        assert_eq!(store.vouchers(company).unwrap().len(), 6);

        let draft = AdvanceDraft {
            kind: InvoiceKind::Sale,
            party_ledger_id: customer,
            bank_ledger_id: bank,
            date: "2026-01-23".into(),
            reference: "STANDALONE".into(),
            amount: 500,
            narration: "Named advances only".into(),
        };
        assert!(store
            .post_party_advance_with_parts(
                company,
                &draft,
                &[
                    parts[0].clone(),
                    AdvancePartDraft {
                        reference: "phase-one".into(),
                        amount: 300
                    },
                ]
            )
            .is_err());
        assert_eq!(store.vouchers(company).unwrap().len(), 6);
        let ids = store
            .post_party_advance_with_parts(company, &draft, &parts)
            .unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ledger(&store, bank), 500);
        let reversal = store
            .reverse_party_advance(
                company,
                ids[0],
                "2026-01-24",
                "ONLY-ADV-REV",
                "Test reversal",
            )
            .unwrap();
        let saved = store
            .party_advances(company, None)
            .unwrap()
            .into_iter()
            .filter(|row| ids.contains(&row.id))
            .collect::<Vec<_>>();
        assert!(saved.iter().all(|row| row.available == 0
            && row.reversal.as_ref().map(|r| r.voucher_id) == Some(reversal)));
        assert_eq!(ledger(&store, bank), 0);
        assert_eq!(store.vouchers(company).unwrap().len(), 8);
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
                <BILLALLOCATIONS.LIST><NAME>FIRST</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>15.00</AMOUNT>
                    <BILLCREDITPERIOD>20260111</BILLCREDITPERIOD></BILLALLOCATIONS.LIST>
                <BILLALLOCATIONS.LIST><NAME>SECOND</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>25.00</AMOUNT>
                    <BILLCREDITPERIOD>30 Days</BILLCREDITPERIOD></BILLALLOCATIONS.LIST>
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
            order: None,
            source_order: 0,
            kind: InvoiceKind::Sale,
            entry_mode: InvoiceEntryMode::Invoice,
            date: date.into(),
            reference: "SELL-01".into(),
            external_reference: Default::default(),
            bills: vec![BillSplitDraft {
                reference: "SELL-01".into(),
                amount: 3000,
                due_date: None,
                credit_days: None,
            }],
            funding_applications: Vec::new(),
            funded_bill_reference: None,
            narration: String::new(),
            party: "Customer".into(),
            existing_party_id: Some(customer),
            trade: "Sales".into(),
            existing_trade_id: Some(sales),
            adjustments: Vec::new(),
            taxes: Vec::new(),
            original_postings: Vec::new(),
            metadata: SourceMetadata::default(),
            lines: vec![ImportedInvoiceLine {
                rate_basis: None,
                item: "Part".into(),
                existing_item_id: Some(item),
                unit: "Each".into(),
                precision: 3,
                trade_ledger: "Sales".into(),
                existing_trade_ledger_id: Some(sales),
                quantity: 3000,
                billed_quantity: 3000,
                unit_price: 1000,
                discount: InvoiceDiscount::None,
                allocations: vec![ImportedStockAllocation {
                    location: "Main".into(),
                    existing_location_id: Some(location),
                    batch: None,
                    existing_batch_id: None,
                    quantity: 3000,
                    billed_quantity: 3000,
                    tracking: None,
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
            stock_notes: Vec::new(),
            stock_returns: Vec::new(),
            stock_counts: Vec::new(),
            stock_journals: Vec::new(),
            cancelled_vouchers: Vec::new(),
            optional_vouchers: Vec::new(),
            orders: Vec::new(),
            stock_preview: None,
            settlements: Vec::new(),
            bill_notes: Vec::new(),
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
