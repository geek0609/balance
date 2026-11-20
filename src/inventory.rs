//! Stock quantities and basic sales/purchase invoices.
//! Quantities use thousandths; prices and accounting amounts use hundredths.
use crate::bill_splits::{BillSplit, BillSplitDraft};
use crate::tax_profiles::{TaxCalculation, TaxCalculationDetail};
use crate::taxes::{InvoiceTaxDetail, InvoiceTaxLine};
use crate::{
    check_ledgers, parse_date, required, validate_voucher, Category, Error, Posting, Result, Store,
    VoucherKind,
};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::{prelude::ToPrimitive, Decimal, RoundingStrategy};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    str::FromStr,
};

const STOCK_LOCATION_BRANCH: &str = "WITH RECURSIVE branch(id) AS (
    SELECT id FROM stock_locations WHERE company_id = ?1 AND id = ?2
    UNION
    SELECT l.id FROM stock_locations l JOIN branch b ON b.id = l.parent_id
    WHERE l.company_id = ?1 AND ?3
)";

pub(crate) fn check_stock_location(conn: &Connection, company: i64, location: i64) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stock_locations WHERE id = ?1 AND company_id = ?2)",
        params![location, company],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(Error::Validation(
            "Stock location not found in the selected company".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct StockItem {
    pub id: i64,
    pub name: String,
    pub unit: String,
    pub unit_id: i64,
    pub group_id: i64,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct StockItemRevision {
    pub changed_at: String,
    pub name: String,
    pub group: String,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct StockUnit {
    pub id: i64,
    pub symbol: String,
    pub formal_name: String,
    /// Precision used by stock movements, in the smallest unit for a compound.
    pub decimal_places: u8,
    pub compound: Option<crate::stock_units::CompoundUnit>,
}
#[derive(Debug, Clone)]
pub struct StockGroup {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct StockLocation {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct StockBatch {
    pub id: i64,
    pub item_id: i64,
    pub name: String,
    pub manufactured_on: Option<String>,
    pub expires_on: Option<String>,
}
#[derive(Debug, Clone)]
pub struct StockBalance {
    pub item: StockItem,
    pub quantity: i128,
}
#[derive(Debug, Clone)]
pub struct StockTransfer {
    pub date: String,
    pub item: String,
    pub unit: String,
    pub batch: Option<String>,
    pub from: String,
    pub to: String,
    pub quantity: i64,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct StockMovement {
    pub date: String,
    pub description: String,
    pub quantity: i64,
    pub running_quantity: i128,
}
#[derive(Debug, Clone)]
pub struct StockRegister {
    pub item: StockItem,
    pub opening_quantity: i128,
    pub movements: Vec<StockMovement>,
    pub closing_quantity: i128,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceKind {
    Sale,
    Purchase,
}
impl InvoiceKind {
    pub const ALL: [Self; 2] = [Self::Sale, Self::Purchase];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sale => "Sale",
            Self::Purchase => "Purchase",
        }
    }
    fn voucher_kind(self) -> VoucherKind {
        match self {
            Self::Sale => VoucherKind::Sales,
            Self::Purchase => VoucherKind::Purchase,
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "Sale" => Ok(Self::Sale),
            "Purchase" => Ok(Self::Purchase),
            _ => Err(Error::Validation("Unknown invoice type".into())),
        }
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceEntryMode {
    #[default]
    Invoice,
    Voucher,
}
impl InvoiceEntryMode {
    pub const ALL: [Self; 2] = [Self::Invoice, Self::Voucher];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invoice => "Invoice",
            Self::Voucher => "Voucher",
        }
    }

    pub fn label(self, kind: InvoiceKind) -> &'static str {
        match (self, kind) {
            (Self::Invoice, _) => kind.as_str(),
            (Self::Voucher, InvoiceKind::Sale) => "Sales voucher",
            (Self::Voucher, InvoiceKind::Purchase) => "Purchase voucher",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "Invoice" => Ok(Self::Invoice),
            "Voucher" => Ok(Self::Voucher),
            _ => Err(Error::Validation("Unknown invoice entry mode".into())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InvoiceLine {
    pub item_id: i64,
    pub location_id: i64,
    pub batch_id: Option<i64>,
    /// None uses the invoice's default sales or purchase ledger.
    pub trade_ledger_id: Option<i64>,
    /// Additional locations or batches; the first location receives the remainder.
    pub extra_allocations: Vec<InvoiceAllocation>,
    /// Quantity shipped or received, in thousandths.
    pub quantity: i64,
    /// Defaults to the stock quantity. Zero records free stock.
    pub billed_quantity: Option<i64>,
    pub unit_price: i64,
    pub rate_basis: Option<InvoiceRateBasis>,
    pub discount: InvoiceDiscount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceRateBasis {
    /// Stock quantity in thousandths covered by the recorded unit price.
    pub quantity: i64,
    pub unit: String,
}
#[derive(Debug, Clone)]
pub enum InvoiceDiscount {
    None,
    Fixed(i64),
    Percent(String),
}
#[derive(Debug, Clone)]
pub struct InvoiceAllocation {
    pub location_id: i64,
    pub batch_id: Option<i64>,
    pub quantity: i64,
    pub billed_quantity: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct InvoiceAllocationDetail {
    pub location: String,
    pub batch: Option<String>,
    pub quantity: i64,
    pub billed_quantity: i64,
}

impl InvoiceAllocationDetail {
    pub fn quantity_label(&self, unit: &str) -> String {
        invoice_quantity_label(self.quantity, self.billed_quantity, unit)
    }
}
#[derive(Debug, Clone)]
pub struct InvoiceAdjustment {
    pub ledger_id: i64,
    /// Positive increases the invoice total; negative decreases it.
    pub amount: i64,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct InvoiceAdjustmentDetail {
    pub ledger: String,
    pub amount: i64,
    pub description: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvoiceExternalReference {
    pub number: Option<String>,
    pub date: Option<String>,
}

pub(crate) fn migrate_external_invoice_references(conn: &Connection) -> Result<()> {
    for table in ["invoices", "invoice_revisions"] {
        for column in ["external_reference", "external_reference_date"] {
            let present: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
                params![table, column],
                |row| row.get(0),
            )?;
            if !present {
                conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"), [])?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct InvoiceDraft {
    pub kind: InvoiceKind,
    pub entry_mode: InvoiceEntryMode,
    pub date: String,
    pub due_date: Option<String>,
    pub credit_days: Option<u32>,
    pub reference: String,
    pub external_reference: InvoiceExternalReference,
    /// Optional reference for settling the bill. Defaults to the invoice reference.
    pub bill_reference: Option<String>,
    pub bill_splits: Vec<BillSplitDraft>,
    pub narration: String,
    pub party_ledger_id: i64,
    pub trade_ledger_id: i64,
    pub tax_ledger_id: Option<i64>,
    pub tax_amount: i64,
    pub additional_taxes: Vec<InvoiceTaxLine>,
    pub tax_calculation: Option<TaxCalculation>,
    pub lines: Vec<InvoiceLine>,
    pub adjustments: Vec<InvoiceAdjustment>,
}
#[derive(Debug, Clone)]
pub struct InvoiceSummary {
    pub id: i64,
    pub voucher_id: i64,
    pub kind: InvoiceKind,
    pub entry_mode: InvoiceEntryMode,
    pub date: String,
    pub due_date: Option<String>,
    pub credit_days: Option<u32>,
    pub reference: String,
    pub external_reference: InvoiceExternalReference,
    pub bill_reference: String,
    pub bill_splits: Vec<BillSplit>,
    pub party: String,
    pub party_ledger_id: i64,
    pub total: i128,
    pub tax_amount: i64,
    pub taxes: Vec<InvoiceTaxDetail>,
    pub tax_calculation: Option<TaxCalculationDetail>,
    pub settled: i128,
    pub advance_applied: i128,
    pub credited: i128,
    pub credit_total: i128,
    pub outstanding: i128,
    pub has_settlement_history: bool,
    pub has_advance_history: bool,
    pub has_credit_history: bool,
    pub has_active_stock_returns: bool,
    pub order_id: Option<i64>,
    pub stock_note_id: Option<i64>,
    pub stock_note_ids: Vec<i64>,
    pub order_ids: Vec<i64>,
    pub opening_bill: Option<crate::opening_bills::OpeningBill>,
    pub reversal: Option<crate::invoice_reversals::InvoiceReversal>,
    pub lines: Vec<InvoiceDetailLine>,
    pub adjustments: Vec<InvoiceAdjustmentDetail>,
}
impl InvoiceSummary {
    pub fn document_type(&self) -> &'static str {
        if self.opening_bill.is_some() {
            crate::opening_bills::label(self.kind)
        } else if self.total == 0 && !self.lines.is_empty() {
            match self.kind {
                InvoiceKind::Sale => "Stock-only sale",
                InvoiceKind::Purchase => "Stock-only purchase",
            }
        } else {
            self.entry_mode.label(self.kind)
        }
    }
}
#[derive(Debug, Clone)]
pub struct Settlement {
    pub id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub reference: String,
    pub amount: i64,
    pub voucher_cash: i64,
    pub voucher_discount: i64,
    pub bank_ledger: String,
    pub reversal_voucher_id: Option<i64>,
    pub reversal_date: Option<String>,
    pub reversal_reason: Option<String>,
    pub bill_allocations: Vec<(String, i64)>,
}
#[derive(Debug, Clone)]
pub struct SettlementAllocation {
    pub invoice_id: i64,
    pub amount: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BillSettlementTarget {
    pub invoice_id: i64,
    pub split_id: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct BillSettlementAllocation {
    pub target: BillSettlementTarget,
    pub amount: i64,
}
#[derive(Debug, Clone)]
pub struct InvoiceRevision {
    pub changed_at: String,
    pub date: String,
    pub due_date: Option<String>,
    pub credit_days: Option<u32>,
    pub kind: InvoiceKind,
    pub entry_mode: InvoiceEntryMode,
    pub reference: String,
    pub external_reference: InvoiceExternalReference,
    pub bill_reference: String,
    pub bill_splits: Vec<BillSplitDraft>,
    pub narration: String,
    pub party: String,
    pub trade: String,
    pub tax_ledger: Option<String>,
    pub tax_amount: i64,
    pub taxes: Vec<InvoiceTaxDetail>,
    pub tax_calculation: Option<TaxCalculationDetail>,
    pub lines: Vec<InvoiceDetailLine>,
    pub adjustments: Vec<InvoiceAdjustmentDetail>,
}
#[derive(Debug, Clone)]
pub struct OutstandingInvoice {
    pub invoice_id: i64,
    pub split_id: Option<i64>,
    pub opening_date: Option<String>,
    pub kind: InvoiceKind,
    pub entry_mode: InvoiceEntryMode,
    pub date: String,
    pub due_date: Option<String>,
    pub reference: String,
    pub bill_reference: String,
    pub party: String,
    pub party_ledger_id: i64,
    pub total: i128,
    pub settled: i128,
    pub advance_applied: i128,
    pub credited: i128,
    pub outstanding: i128,
    /// Days since the invoice date, not days past a payment due date.
    pub age_days: i64,
    /// Days after the due date, if one is set. Zero means not overdue yet.
    pub overdue_days: Option<i64>,
}
#[derive(Debug, Clone, Default)]
pub struct PartyOutstanding {
    pub party_ledger_id: i64,
    pub party: String,
    pub receivable: i128,
    pub payable: i128,
    pub overdue_receivable: i128,
    pub overdue_payable: i128,
    /// Unallocated opening balance, positive for debit and negative for credit.
    pub opening_on_account: i128,
    pub invoices: usize,
}
#[derive(Debug, Clone)]
pub struct InvoiceDetailLine {
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub trade_ledger: String,
    pub allocations: Vec<InvoiceAllocationDetail>,
    pub quantity: i64,
    pub billed_quantity: i64,
    pub unit_price: i64,
    pub rate_basis: Option<InvoiceRateBasis>,
    pub amount: i64,
    pub discount: InvoiceDiscount,
    pub discount_amount: i64,
}

impl InvoiceDetailLine {
    pub fn quantity_label(&self) -> String {
        invoice_quantity_label(self.quantity, self.billed_quantity, &self.unit)
    }

    pub fn price_label(&self) -> String {
        let price = crate::format_amount(self.unit_price);
        match &self.rate_basis {
            Some(basis) => format!("{price} / {}", basis.unit),
            None => price,
        }
    }
}

fn invoice_quantity_label(quantity: i64, billed_quantity: i64, unit: &str) -> String {
    let stock = format_quantity(quantity.into());
    if billed_quantity == quantity {
        format!("{stock} {unit}")
    } else {
        let billed = format_quantity(billed_quantity.into());
        format!("{stock} {unit} stock · {billed} billed")
    }
}

pub fn parse_quantity(input: &str) -> Result<i64> {
    let decimal = Decimal::from_str(input.trim())
        .map_err(|_| Error::Validation("Enter a valid quantity".into()))?;
    let scaled = decimal
        .checked_mul(Decimal::from(1000))
        .ok_or_else(|| Error::Validation("Quantity is too large".into()))?;
    if decimal < Decimal::ZERO || !scaled.fract().is_zero() {
        return Err(Error::Validation(
            "Quantities must be non-negative with at most three decimal places".into(),
        ));
    }
    scaled
        .to_i64()
        .ok_or_else(|| Error::Validation("Quantity is too large".into()))
}
pub fn format_quantity(quantity: i128) -> String {
    let n = quantity.unsigned_abs();
    format!(
        "{}{}.{:03}",
        if quantity < 0 { "-" } else { "" },
        n / 1000,
        n % 1000
    )
}
/// Multiply a positive quantity in thousandths by a unit price in hundredths,
/// rounding half up to one hundredth per line.
pub fn line_amount(quantity: i64, unit_price: i64) -> Result<i64> {
    if quantity <= 0 {
        return Err(Error::Validation(
            "Each line needs a positive quantity".into(),
        ));
    }
    line_amount_with_basis(quantity, unit_price, None)
}

fn line_amount_with_basis(
    quantity: i64,
    unit_price: i64,
    rate_basis: Option<&InvoiceRateBasis>,
) -> Result<i64> {
    if quantity < 0 || unit_price < 0 {
        return Err(Error::Validation(
            "Billed quantity and price must be non-negative".into(),
        ));
    }
    let basis = match rate_basis {
        None => 1000,
        Some(basis) if basis.quantity > 0 && !basis.unit.trim().is_empty() => {
            i128::from(basis.quantity)
        }
        Some(_) => {
            return Err(Error::Validation(
                "An invoice rate needs a positive quantity and unit".into(),
            ))
        }
    };
    let amount = (i128::from(quantity) * i128::from(unit_price) + basis / 2) / basis;
    i64::try_from(amount).map_err(|_| Error::Validation("Invoice line amount is too large".into()))
}
/// Discount is applied to the gross line amount after its monetary rounding.
pub fn discounted_line_amount(
    quantity: i64,
    unit_price: i64,
    discount: &InvoiceDiscount,
) -> Result<(i64, i64)> {
    if quantity <= 0 {
        return Err(Error::Validation(
            "Each line needs a positive quantity".into(),
        ));
    }
    discounted_line_amount_with_basis(quantity, unit_price, discount, None)
}

pub fn discounted_line_amount_with_basis(
    quantity: i64,
    unit_price: i64,
    discount: &InvoiceDiscount,
    rate_basis: Option<&InvoiceRateBasis>,
) -> Result<(i64, i64)> {
    let gross = line_amount_with_basis(quantity, unit_price, rate_basis)?;
    let amount = match discount {
        InvoiceDiscount::None => 0,
        InvoiceDiscount::Fixed(value) => *value,
        InvoiceDiscount::Percent(value) => {
            let rate = Decimal::from_str(value.trim())
                .map_err(|_| Error::Validation("Enter a valid discount percentage".into()))?
                .normalize();
            if rate < Decimal::ZERO || rate > Decimal::from(100) || rate.scale() > 4 {
                return Err(Error::Validation("Discount percentage must be between 0 and 100 with at most four decimal places".into()));
            }
            Decimal::from(gross)
                .checked_mul(rate)
                .and_then(|value| value.checked_div(Decimal::from(100)))
                .ok_or_else(|| Error::Validation("Discount is too large".into()))?
                .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
                .to_i64()
                .ok_or_else(|| Error::Validation("Discount is too large".into()))?
        }
    };
    if amount < 0 || amount > gross {
        return Err(Error::Validation(
            "Discount must not exceed the gross item amount".into(),
        ));
    }
    Ok((gross - amount, amount))
}
fn discount_parts(discount: &InvoiceDiscount) -> (&'static str, String) {
    match discount {
        InvoiceDiscount::None => ("None", String::new()),
        InvoiceDiscount::Fixed(amount) => ("Fixed", amount.to_string()),
        InvoiceDiscount::Percent(rate) => ("Percent", rate.trim().to_owned()),
    }
}
fn parse_discount(kind: &str, value: &str) -> Result<InvoiceDiscount> {
    match kind {
        "None" if value.is_empty() => Ok(InvoiceDiscount::None),
        "Fixed" => {
            Ok(InvoiceDiscount::Fixed(value.parse::<i64>().map_err(
                |_| Error::Validation("Invalid saved discount".into()),
            )?))
        }
        "Percent" => Ok(InvoiceDiscount::Percent(value.to_owned())),
        _ => Err(Error::Validation("Invalid saved discount".into())),
    }
}
struct PreparedInvoice {
    date: String,
    due_date: Option<String>,
    credit_days: Option<u32>,
    reference: String,
    external_reference: InvoiceExternalReference,
    bill_reference: Option<String>,
    bill_splits: Vec<BillSplitDraft>,
    postings: Vec<Posting>,
    amounts: Vec<i64>,
    discounts: Vec<i64>,
}
fn prepare_invoice(
    conn: &Connection,
    company_id: i64,
    draft: &InvoiceDraft,
    excluding: Option<i64>,
) -> Result<PreparedInvoice> {
    let invoice_date = parse_date(&draft.date)?;
    let date = invoice_date.format("%Y-%m-%d").to_string();
    let external_reference = InvoiceExternalReference {
        number: draft
            .external_reference
            .number
            .as_deref()
            .map(str::trim)
            .filter(|number| !number.is_empty())
            .map(str::to_owned),
        date: draft
            .external_reference
            .date
            .as_deref()
            .map(parse_date)
            .transpose()?
            .map(|date| date.format("%Y-%m-%d").to_string()),
    };
    if draft.credit_days.is_some() && draft.due_date.is_some() {
        return Err(Error::Validation(
            "Enter either a due date or a credit period, not both".into(),
        ));
    }
    let due_date = if let Some(days) = draft.credit_days {
        Some(
            invoice_date
                .checked_add_days(chrono::Days::new(u64::from(days)))
                .ok_or_else(|| {
                    Error::Validation("Credit period produces an invalid due date".into())
                })?,
        )
    } else {
        draft.due_date.as_deref().map(parse_date).transpose()?
    }
    .map(|date| date.format("%Y-%m-%d").to_string());
    let reference = required(&draft.reference, "Invoice reference")?.to_owned();
    let bill_reference = draft
        .bill_reference
        .as_deref()
        .map(|value| required(value, "Bill reference"))
        .transpose()?
        .filter(|value| *value != reference)
        .map(str::to_owned);
    if !draft.bill_splits.is_empty() && (draft.bill_reference.is_some() || due_date.is_some()) {
        return Err(Error::Validation(
            "Enter references and due dates on the split bills".into(),
        ));
    }
    if draft.lines.is_empty() && draft.adjustments.is_empty() {
        return Err(Error::Validation(
            "An invoice needs at least one item or service / charge line".into(),
        ));
    }
    if draft.party_ledger_id == draft.trade_ledger_id {
        return Err(Error::Validation(
            "The party and trade ledgers must be different".into(),
        ));
    }
    let taxes = crate::taxes::validate_draft_taxes(conn, company_id, draft)?;
    let tax_ledgers = taxes
        .iter()
        .map(|line| line.ledger_id)
        .collect::<HashSet<_>>();
    let total_tax = crate::taxes::checked_total(taxes.iter().map(|line| line.amount))?;
    let party = ledger_category(conn, company_id, draft.party_ledger_id)?;
    let trade = ledger_category(conn, company_id, draft.trade_ledger_id)?;
    if !matches!(party, Category::Asset | Category::Liability) {
        return Err(Error::Validation(
            "The party ledger must be an asset or liability".into(),
        ));
    }
    if (draft.kind == InvoiceKind::Sale && trade != Category::Income)
        || (draft.kind == InvoiceKind::Purchase
            && !matches!(trade, Category::Expense | Category::Asset))
    {
        return Err(Error::Validation(
            "The trade ledger has an incompatible category".into(),
        ));
    }
    let mut amounts = Vec::with_capacity(draft.lines.len());
    let mut discounts = Vec::with_capacity(draft.lines.len());
    let mut subtotal = 0_i128;
    let mut trade_totals = BTreeMap::<i64, i128>::new();
    for line in &draft.lines {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
            params![line.item_id, company_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Every item must belong to the selected company".into(),
            ));
        }
        let location_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_locations WHERE id = ?1 AND company_id = ?2)",
            params![line.location_id, company_id],
            |r| r.get(0),
        )?;
        if !location_exists {
            return Err(Error::Validation(
                "Every stock location must belong to the selected company".into(),
            ));
        }
        check_stock_batch(conn, company_id, line.item_id, line.batch_id)?;
        check_quantity_precision(conn, company_id, line.item_id, line.quantity)?;
        let billed_quantity = line.billed_quantity.unwrap_or(line.quantity);
        if billed_quantity != line.quantity {
            check_quantity_precision(conn, company_id, line.item_id, billed_quantity)?;
        }
        for allocation in &line.extra_allocations {
            if allocation.quantity <= 0 {
                return Err(Error::Validation(
                    "Allocation quantity must be positive".into(),
                ));
            }
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM stock_locations WHERE id = ?1 AND company_id = ?2)",
                params![allocation.location_id, company_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Validation(
                    "Every allocation location must belong to the selected company".into(),
                ));
            }
            check_stock_batch(conn, company_id, line.item_id, allocation.batch_id)?;
            check_quantity_precision(conn, company_id, line.item_id, allocation.quantity)?;
            let billed = allocation.billed_quantity.unwrap_or(allocation.quantity);
            if billed < 0 {
                return Err(Error::Validation(
                    "Allocation billed quantity must be non-negative".into(),
                ));
            }
            if billed != allocation.quantity {
                check_quantity_precision(conn, company_id, line.item_id, billed)?;
            }
        }
        primary_invoice_allocation(line)?;
        let trade_id = line.trade_ledger_id.unwrap_or(draft.trade_ledger_id);
        if trade_id == draft.party_ledger_id || tax_ledgers.contains(&trade_id) {
            return Err(Error::Validation(
                "An item ledger must differ from the party and tax ledgers".into(),
            ));
        }
        let category = ledger_category(conn, company_id, trade_id)?;
        if (draft.kind == InvoiceKind::Sale && category != Category::Income)
            || (draft.kind == InvoiceKind::Purchase
                && !matches!(category, Category::Expense | Category::Asset))
        {
            return Err(Error::Validation(
                "An item ledger has an incompatible category".into(),
            ));
        }
        let (amount, discount) = discounted_line_amount_with_basis(
            billed_quantity,
            line.unit_price,
            &line.discount,
            line.rate_basis.as_ref(),
        )?;
        subtotal += i128::from(amount);
        *trade_totals.entry(trade_id).or_default() += i128::from(amount);
        amounts.push(amount);
        discounts.push(discount);
    }
    let subtotal = i64::try_from(subtotal)
        .map_err(|_| Error::Validation("Invoice amount is too large".into()))?;
    let mut total = i128::from(subtotal) + i128::from(total_tax);
    let mut has_service_base = false;
    for adjustment in &draft.adjustments {
        if adjustment.amount == 0
            || adjustment.amount == i64::MIN
            || adjustment.ledger_id == draft.party_ledger_id
        {
            return Err(Error::Validation(
                "Each adjustment needs a non-zero amount and a ledger different from the party"
                    .into(),
            ));
        }
        let category = ledger_category(conn, company_id, adjustment.ledger_id)?;
        if adjustment.amount > 0
            && ((draft.kind == InvoiceKind::Sale && category == Category::Income)
                || (draft.kind == InvoiceKind::Purchase
                    && matches!(category, Category::Expense | Category::Asset)))
        {
            has_service_base = true;
        }
        total += i128::from(adjustment.amount);
    }
    if draft.lines.is_empty() && !has_service_base {
        return Err(Error::Validation(
            "A non-stock invoice needs a positive service / charge line in a compatible sales or purchase account".into(),
        ));
    }
    if let Some(context) = draft.tax_calculation {
        crate::tax_profiles::validate_invoice_calculation(
            conn,
            company_id,
            draft.kind,
            invoice_date,
            total - i128::from(total_tax),
            context,
            &taxes,
        )?;
    }
    let stock_only =
        total == 0 && !draft.lines.is_empty() && draft.adjustments.is_empty() && taxes.is_empty();
    if total <= 0 && !stock_only {
        return Err(Error::Validation(
            "Invoice amount must be positive unless the invoice only moves free stock".into(),
        ));
    }
    if stock_only && (draft.bill_reference.is_some() || !draft.bill_splits.is_empty()) {
        return Err(Error::Validation(
            "A stock-only invoice cannot create bills for settlement".into(),
        ));
    }
    let total = i64::try_from(total)
        .map_err(|_| Error::Validation("Invoice amount is too large".into()))?;
    let bill_splits = crate::bill_splits::prepare(&draft.bill_splits, invoice_date, total)?;
    let bill_references = if stock_only {
        Vec::new()
    } else if bill_splits.is_empty() {
        vec![bill_reference.as_deref().unwrap_or(&reference)]
    } else {
        bill_splits
            .iter()
            .map(|bill| bill.reference.as_str())
            .collect()
    };
    check_invoice_references(
        conn,
        company_id,
        draft.kind,
        draft.party_ledger_id,
        &reference,
        &bill_references,
        excluding,
    )?;
    let direction = if draft.kind == InvoiceKind::Sale {
        1
    } else {
        -1
    };
    let mut postings = Vec::new();
    if !stock_only {
        postings.push(Posting {
            ledger_id: draft.party_ledger_id,
            amount: direction * total,
        });
    }
    for (ledger_id, amount) in trade_totals {
        if amount != 0 {
            let amount = i64::try_from(amount)
                .map_err(|_| Error::Validation("Item ledger total is too large".into()))?;
            postings.push(Posting {
                ledger_id,
                amount: -direction * amount,
            });
        }
    }
    for tax in taxes {
        postings.push(Posting {
            ledger_id: tax.ledger_id,
            amount: -direction * tax.amount,
        });
    }
    for adjustment in &draft.adjustments {
        postings.push(Posting {
            ledger_id: adjustment.ledger_id,
            amount: -direction * adjustment.amount,
        });
    }
    if !stock_only {
        validate_voucher(&date, &postings)?;
        check_ledgers(conn, company_id, &postings)?;
        crate::opening::check_no_control_postings(conn, company_id, &postings)?;
    }
    Ok(PreparedInvoice {
        date,
        due_date,
        credit_days: draft.credit_days,
        reference,
        external_reference,
        bill_reference,
        bill_splits,
        postings,
        amounts,
        discounts,
    })
}

pub(crate) fn migrate_invoice_rate_basis(conn: &Connection) -> Result<()> {
    for table in ["invoice_lines", "invoice_revision_lines"] {
        for (column, definition) in [
            ("rate_basis_quantity", "INTEGER CHECK(rate_basis_quantity > 0)"),
            ("rate_basis_unit", "TEXT CHECK((rate_basis_quantity IS NULL AND rate_basis_unit IS NULL) OR (rate_basis_quantity IS NOT NULL AND rate_basis_unit IS NOT NULL AND length(trim(rate_basis_unit)) > 0))"),
        ] {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
                params![table, column],
                |row| row.get(0),
            )?;
            if !exists {
                conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"), [])?;
            }
        }
    }
    Ok(())
}

pub(crate) fn migrate_invoice_billed_quantities(conn: &Connection) -> Result<()> {
    for table in [
        "invoice_lines",
        "invoice_revision_lines",
        "invoice_line_allocations",
        "invoice_revision_line_allocations",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'billed_quantity')",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN billed_quantity INTEGER CHECK(billed_quantity >= 0)"),
                [],
            )?;
        }
    }
    Ok(())
}

fn read_invoice_rate_basis(
    row: &rusqlite::Row<'_>,
    quantity: usize,
    unit: usize,
) -> rusqlite::Result<Option<InvoiceRateBasis>> {
    row.get::<_, Option<i64>>(quantity)?
        .map(|quantity| {
            Ok(InvoiceRateBasis {
                quantity,
                unit: row.get(unit)?,
            })
        })
        .transpose()
}
pub(crate) fn check_invoice_references(
    conn: &Connection,
    company_id: i64,
    kind: InvoiceKind,
    party: i64,
    reference: &str,
    bill_references: &[&str],
    excluding: Option<i64>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT v.reference, CASE WHEN EXISTS(SELECT 1 FROM postings p WHERE p.voucher_id = v.id)
                THEN COALESCE(b.reference, i.bill_reference, v.reference) END
         FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
         LEFT JOIN invoice_bill_splits b ON b.invoice_id = i.id
         WHERE i.company_id = ?1 AND i.kind = ?2 AND i.party_ledger_id = ?3
               AND (?4 IS NULL OR i.id != ?4)",
    )?;
    let key = reference.to_lowercase();
    let bill_keys: HashSet<_> = bill_references
        .iter()
        .map(|reference| reference.to_lowercase())
        .collect();
    for existing in stmt.query_map(params![company_id, kind.as_str(), party, excluding], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })? {
        let (reference, bill_reference) = existing?;
        if reference.trim().to_lowercase() == key {
            return Err(Error::Validation(
                "An invoice with this type, party and reference already exists".into(),
            ));
        }
        if bill_reference.is_some_and(|bill| bill_keys.contains(&bill.trim().to_lowercase())) {
            return Err(Error::Validation(
                "A bill with this type, party and reference already exists".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn write_postings(conn: &Connection, voucher: i64, postings: &[Posting]) -> Result<()> {
    for posting in postings {
        conn.execute(
            "INSERT INTO postings(voucher_id, ledger_id, amount) VALUES (?1, ?2, ?3)",
            params![voucher, posting.ledger_id, posting.amount],
        )?;
    }
    Ok(())
}
fn primary_invoice_allocation(line: &InvoiceLine) -> Result<InvoiceAllocation> {
    let extra_quantity: i128 = line
        .extra_allocations
        .iter()
        .map(|a| i128::from(a.quantity))
        .sum();
    let extra_billed: i128 = line
        .extra_allocations
        .iter()
        .map(|a| i128::from(a.billed_quantity.unwrap_or(a.quantity)))
        .sum();
    let quantity = i64::try_from(i128::from(line.quantity) - extra_quantity).ok()
        .filter(|quantity| *quantity > 0)
        .ok_or_else(|| Error::Validation("Additional allocations must leave a positive stock quantity for the first location".into()))?;
    let billed_quantity =
        i64::try_from(i128::from(line.billed_quantity.unwrap_or(line.quantity)) - extra_billed)
            .ok()
            .filter(|quantity| *quantity >= 0)
            .ok_or_else(|| {
                Error::Validation(
                    "Billed allocations exceed the invoice line billed quantity".into(),
                )
            })?;
    Ok(InvoiceAllocation {
        location_id: line.location_id,
        batch_id: line.batch_id,
        quantity,
        billed_quantity: (billed_quantity != quantity).then_some(billed_quantity),
    })
}

fn write_invoice_lines(
    conn: &Connection,
    invoice: i64,
    lines: &[InvoiceLine],
    amounts: &[i64],
    discounts: &[i64],
) -> Result<()> {
    if lines.len() != amounts.len() || lines.len() != discounts.len() {
        return Err(Error::Validation(
            "Invoice line calculations are inconsistent".into(),
        ));
    }
    for ((line, amount), discount) in lines.iter().zip(amounts).zip(discounts) {
        let (discount_kind, discount_value) = discount_parts(&line.discount);
        conn.execute(
            "INSERT INTO invoice_lines(invoice_id, item_id, quantity, unit_price, amount,
                location_id, batch_id, trade_ledger_id, discount_kind, discount_value, discount_amount,
                rate_basis_quantity, rate_basis_unit, billed_quantity) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![invoice, line.item_id, line.quantity, line.unit_price, amount, line.location_id,
                line.batch_id, line.trade_ledger_id, discount_kind, discount_value, discount,
                line.rate_basis.as_ref().map(|basis| basis.quantity),
                line.rate_basis.as_ref().map(|basis| basis.unit.as_str()), line.billed_quantity],
        )?;
        let line_id = conn.last_insert_rowid();
        let primary = primary_invoice_allocation(line)?;
        for allocation in std::iter::once(&primary).chain(&line.extra_allocations) {
            conn.execute(
                "INSERT INTO invoice_line_allocations(line_id, location_id, batch_id, quantity, billed_quantity)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![line_id, allocation.location_id, allocation.batch_id, allocation.quantity, allocation.billed_quantity],
            )?;
        }
    }
    Ok(())
}
fn read_line_allocations(
    conn: &Connection,
    line: i64,
    revision: bool,
) -> Result<Vec<InvoiceAllocation>> {
    let sql = if revision {
        "SELECT location_id, batch_id, quantity, billed_quantity FROM invoice_revision_line_allocations WHERE revision_line_id = ?1 ORDER BY id"
    } else {
        "SELECT location_id, batch_id, quantity, billed_quantity FROM invoice_line_allocations WHERE line_id = ?1 ORDER BY id"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([line], |r| {
            Ok(InvoiceAllocation {
                location_id: r.get(0)?,
                batch_id: r.get(1)?,
                quantity: r.get(2)?,
                billed_quantity: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
fn check_line_allocations(
    allocations: &[InvoiceAllocation],
    quantity: i64,
    billed_quantity: i64,
    location: i64,
    batch: Option<i64>,
) -> Result<()> {
    if allocations
        .first()
        .is_none_or(|first| first.location_id != location || first.batch_id != batch)
        || allocations.iter().any(|allocation| {
            allocation.quantity <= 0
                || allocation.billed_quantity.unwrap_or(allocation.quantity) < 0
        })
        || allocations
            .iter()
            .map(|allocation| i128::from(allocation.quantity))
            .sum::<i128>()
            != i128::from(quantity)
        || allocations
            .iter()
            .map(|a| i128::from(a.billed_quantity.unwrap_or(a.quantity)))
            .sum::<i128>()
            != i128::from(billed_quantity)
    {
        return Err(Error::Validation(
            "Invoice stock allocations are inconsistent".into(),
        ));
    }
    Ok(())
}
fn read_allocation_details(
    conn: &Connection,
    line: i64,
    revision: bool,
) -> Result<Vec<InvoiceAllocationDetail>> {
    let sql = if revision {
        "SELECT loc.name, b.name, a.quantity, COALESCE(a.billed_quantity, a.quantity) FROM invoice_revision_line_allocations a JOIN stock_locations loc ON loc.id = a.location_id LEFT JOIN stock_batches b ON b.id = a.batch_id WHERE a.revision_line_id = ?1 ORDER BY a.id"
    } else {
        "SELECT loc.name, b.name, a.quantity, COALESCE(a.billed_quantity, a.quantity) FROM invoice_line_allocations a JOIN stock_locations loc ON loc.id = a.location_id LEFT JOIN stock_batches b ON b.id = a.batch_id WHERE a.line_id = ?1 ORDER BY a.id"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([line], |r| {
            Ok(InvoiceAllocationDetail {
                location: r.get(0)?,
                batch: r.get(1)?,
                quantity: r.get(2)?,
                billed_quantity: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
fn attach_allocation_details(
    conn: &Connection,
    id: i64,
    revision: bool,
    mut line: InvoiceDetailLine,
) -> Result<InvoiceDetailLine> {
    let sql = if revision {
        "SELECT discount_kind, discount_value, discount_amount, rate_basis_quantity, rate_basis_unit, COALESCE(billed_quantity, quantity) FROM invoice_revision_lines WHERE id = ?1"
    } else {
        "SELECT discount_kind, discount_value, discount_amount, rate_basis_quantity, rate_basis_unit, COALESCE(billed_quantity, quantity) FROM invoice_lines WHERE id = ?1"
    };
    let (kind, value, saved_amount, rate_basis, billed_quantity): (String, String, i64, _, i64) =
        conn.query_row(sql, [id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                read_invoice_rate_basis(r, 3, 4)?,
                r.get(5)?,
            ))
        })?;
    let discount = parse_discount(&kind, &value)?;
    if discounted_line_amount_with_basis(
        billed_quantity,
        line.unit_price,
        &discount,
        rate_basis.as_ref(),
    )? != (line.amount, saved_amount)
    {
        return Err(Error::Validation(
            "Invoice discount calculation is inconsistent".into(),
        ));
    }
    line.billed_quantity = billed_quantity;
    line.rate_basis = rate_basis;
    line.discount = discount;
    line.discount_amount = saved_amount;
    let allocations = read_allocation_details(conn, id, revision)?;
    if allocations
        .first()
        .is_none_or(|first| first.location != line.location || first.batch != line.batch)
        || allocations
            .iter()
            .any(|allocation| allocation.quantity <= 0 || allocation.billed_quantity < 0)
        || allocations
            .iter()
            .map(|allocation| i128::from(allocation.quantity))
            .sum::<i128>()
            != i128::from(line.quantity)
        || allocations
            .iter()
            .map(|a| i128::from(a.billed_quantity))
            .sum::<i128>()
            != i128::from(billed_quantity)
    {
        return Err(Error::Validation(
            "Invoice stock allocations are inconsistent".into(),
        ));
    }
    line.allocations = allocations;
    Ok(line)
}
fn copy_invoice_line_revisions(conn: &Connection, invoice: i64, revision: i64) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id, item_id, quantity, unit_price, amount, location_id, batch_id, trade_ledger_id, discount_kind, discount_value, discount_amount, rate_basis_quantity, rate_basis_unit, billed_quantity FROM invoice_lines WHERE invoice_id = ?1 ORDER BY id")?;
    let lines = stmt
        .query_map([invoice], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, i64>(10)?,
                read_invoice_rate_basis(r, 11, 12)?,
                r.get::<_, Option<i64>>(13)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (
        id,
        item,
        quantity,
        unit_price,
        amount,
        location,
        batch,
        trade,
        discount_kind,
        discount_value,
        discount_amount,
        rate_basis,
        billed_quantity,
    ) in lines
    {
        let allocations = read_line_allocations(conn, id, false)?;
        check_line_allocations(
            &allocations,
            quantity,
            billed_quantity.unwrap_or(quantity),
            location,
            batch,
        )?;
        let discount = parse_discount(&discount_kind, &discount_value)?;
        if discounted_line_amount_with_basis(
            billed_quantity.unwrap_or(quantity),
            unit_price,
            &discount,
            rate_basis.as_ref(),
        )? != (amount, discount_amount)
        {
            return Err(Error::Validation(
                "Invoice discount calculation is inconsistent".into(),
            ));
        }
        conn.execute(
            "INSERT INTO invoice_revision_lines(revision_id, item_id, quantity, unit_price, amount, location_id,
                batch_id, trade_ledger_id, discount_kind, discount_value, discount_amount, rate_basis_quantity, rate_basis_unit, billed_quantity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![revision, item, quantity, unit_price, amount, location, batch, trade, discount_kind,
                discount_value, discount_amount, rate_basis.as_ref().map(|basis| basis.quantity),
                rate_basis.as_ref().map(|basis| basis.unit.as_str()), billed_quantity],
        )?;
        let revision_line = conn.last_insert_rowid();
        for allocation in allocations {
            conn.execute("INSERT INTO invoice_revision_line_allocations(revision_line_id, location_id, batch_id, quantity, billed_quantity) VALUES (?1, ?2, ?3, ?4, ?5)", params![revision_line, allocation.location_id, allocation.batch_id, allocation.quantity, allocation.billed_quantity])?;
        }
    }
    Ok(())
}
fn write_invoice_adjustments(
    conn: &Connection,
    invoice: i64,
    adjustments: &[InvoiceAdjustment],
) -> Result<()> {
    for adjustment in adjustments {
        conn.execute("INSERT INTO invoice_adjustments(invoice_id, ledger_id, amount, description) VALUES (?1, ?2, ?3, ?4)", params![invoice, adjustment.ledger_id, adjustment.amount, adjustment.description.trim()])?;
    }
    Ok(())
}
fn read_invoice_adjustments(
    conn: &Connection,
    id: i64,
    revision: bool,
) -> Result<Vec<InvoiceAdjustmentDetail>> {
    let sql = if revision {
        "SELECT l.name, a.amount, a.description FROM invoice_revision_adjustments a JOIN ledgers l ON l.id = a.ledger_id WHERE a.revision_id = ?1 ORDER BY a.id"
    } else {
        "SELECT l.name, a.amount, a.description FROM invoice_adjustments a JOIN ledgers l ON l.id = a.ledger_id WHERE a.invoice_id = ?1 ORDER BY a.id"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([id], |r| {
            Ok(InvoiceAdjustmentDetail {
                ledger: r.get(0)?,
                amount: r.get(1)?,
                description: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
/// Insert an invoice using the caller's transaction, including its stock lines and voucher.
pub(crate) fn insert_invoice_on_connection(
    conn: &Connection,
    company_id: i64,
    draft: &InvoiceDraft,
) -> Result<(i64, i64)> {
    insert_invoice(conn, company_id, draft, None)
}

fn insert_invoice(
    conn: &Connection,
    company_id: i64,
    draft: &InvoiceDraft,
    original_postings: Option<&[Posting]>,
) -> Result<(i64, i64)> {
    let prepared = prepare_invoice(conn, company_id, draft, None)?;
    let postings = if let Some(original) = original_postings {
        if !original.is_empty() || !prepared.postings.is_empty() {
            validate_voucher(&prepared.date, original)?;
            check_ledgers(conn, company_id, original)?;
        }
        let totals = |postings: &[Posting]| {
            let mut totals = BTreeMap::<i64, i128>::new();
            for posting in postings {
                *totals.entry(posting.ledger_id).or_default() += i128::from(posting.amount);
            }
            totals.retain(|_, amount| *amount != 0);
            totals
        };
        if totals(original) != totals(&prepared.postings) {
            return Err(Error::Validation(
                "The imported ledger postings do not match the bill and stock amounts".into(),
            ));
        }
        original
    } else {
        &prepared.postings
    };
    conn.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, draft.kind.voucher_kind().as_str(), prepared.reference, prepared.date, draft.narration.trim()])?;
    let voucher_id = conn.last_insert_rowid();
    write_invoice_postings(conn, voucher_id, draft, postings)?;
    conn.execute(
        "INSERT INTO invoices(company_id, voucher_id, kind, party_ledger_id, trade_ledger_id,
             tax_ledger_id, tax_amount, due_date, credit_days, bill_reference, entry_mode,
             external_reference, external_reference_date)
          VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            company_id,
            voucher_id,
            draft.kind.as_str(),
            draft.party_ledger_id,
            draft.trade_ledger_id,
            draft.tax_ledger_id,
            draft.tax_amount,
            prepared.due_date,
            prepared.credit_days,
            prepared.bill_reference,
            draft.entry_mode.as_str(),
            prepared.external_reference.number,
            prepared.external_reference.date,
        ],
    )?;
    let invoice_id = conn.last_insert_rowid();
    crate::bill_splits::write(conn, invoice_id, &prepared.bill_splits)?;
    write_invoice_lines(
        conn,
        invoice_id,
        &draft.lines,
        &prepared.amounts,
        &prepared.discounts,
    )?;
    write_invoice_adjustments(conn, invoice_id, &draft.adjustments)?;
    crate::taxes::write_additional(conn, invoice_id, &draft.additional_taxes)?;
    crate::tax_profiles::write_calculation(conn, invoice_id, draft.tax_calculation)?;
    Ok((invoice_id, voucher_id))
}

pub(crate) fn is_stock_only_invoice(conn: &Connection, company: i64, invoice: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM invoices i WHERE i.company_id = ?1 AND i.id = ?2 AND i.tax_amount = 0
         AND EXISTS(SELECT 1 FROM invoice_lines l WHERE l.invoice_id = i.id)
         AND NOT EXISTS(SELECT 1 FROM invoice_lines l WHERE l.invoice_id = i.id AND l.amount != 0)
         AND NOT EXISTS(SELECT 1 FROM invoice_adjustments a WHERE a.invoice_id = i.id)
         AND NOT EXISTS(SELECT 1 FROM invoice_additional_taxes t WHERE t.invoice_id = i.id))",
        params![company, invoice],
        |row| row.get(0),
    )?)
}

fn write_invoice_postings(
    conn: &Connection,
    voucher: i64,
    draft: &InvoiceDraft,
    postings: &[Posting],
) -> Result<()> {
    if draft.entry_mode == InvoiceEntryMode::Voucher {
        let party_lines: Vec<_> = postings
            .iter()
            .enumerate()
            .filter_map(|(index, posting)| {
                (posting.ledger_id == draft.party_ledger_id).then_some(index)
            })
            .collect();
        crate::write_voucher_postings(conn, voucher, postings, &party_lines)
    } else {
        write_postings(conn, voucher, postings)
    }
}

fn invoice_stock_allocations(
    conn: &Connection,
    invoice: i64,
) -> Result<Vec<(i64, i64, Option<i64>, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT l.item_id, a.location_id, a.batch_id, a.quantity
         FROM physical_invoice_lines l JOIN invoice_line_allocations a ON a.line_id = l.id
         WHERE l.invoice_id = ?1",
    )?;
    let rows = stmt.query_map([invoice], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) enum StockInvoicePosting<'a> {
    Direct,
    Order(i64),
    Import {
        original_postings: Option<&'a [Posting]>,
    },
}

pub(crate) fn insert_direct_invoice(
    conn: &Connection,
    company: i64,
    draft: &InvoiceDraft,
    posting: StockInvoicePosting<'_>,
) -> Result<(i64, i64)> {
    let original_postings = match &posting {
        StockInvoicePosting::Import { original_postings } => *original_postings,
        _ => None,
    };
    let ids = insert_invoice(conn, company, draft, original_postings)?;
    if draft.kind == InvoiceKind::Sale {
        let allocations = invoice_stock_allocations(conn, ids.0)?;
        let mut buckets = HashSet::new();
        let reference = format!("Invoice {} (#{})", draft.reference.trim(), ids.0);
        for (item, location, batch, quantity) in allocations {
            if let StockInvoicePosting::Order(order) = posting {
                crate::stock_reservations::consume_reservations(
                    conn, order, item, location, batch, quantity, &reference,
                )?;
            }
            buckets.insert((item, location, batch));
        }
        for (item, location, batch) in buckets {
            crate::stock_reservations::check_stock_coverage(conn, company, item, location, batch)?;
            if !matches!(posting, StockInvoicePosting::Import { .. }) {
                crate::stock_policy::check_bucket(conn, company, item, location, batch)?;
            }
        }
    }
    Ok(ids)
}

pub(crate) fn ledger_category(
    conn: &Connection,
    company_id: i64,
    ledger_id: i64,
) -> Result<Category> {
    let value: String = conn
        .query_row(
            "SELECT category FROM ledgers WHERE id = ?1 AND company_id = ?2",
            params![ledger_id, company_id],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Validation("Ledger not found in the selected company".into())
            }
            other => Error::Database(other),
        })?;
    Category::parse(&value)
}
pub(crate) fn seed_stock_groups(conn: &Connection, company_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO stock_groups(company_id, name, parent_id) VALUES (?1, 'Primary', NULL)",
        [company_id],
    )?;
    Ok(())
}
pub(crate) fn seed_stock_locations(conn: &Connection, company_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO stock_locations(company_id, name) VALUES (?1, 'Main')",
        [company_id],
    )?;
    Ok(())
}
pub(crate) fn main_stock_location(conn: &Connection, company_id: i64) -> Result<i64> {
    conn.query_row(
        "SELECT id FROM stock_locations WHERE company_id = ?1 AND name = 'Main'",
        [company_id],
        |r| r.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            Error::Validation("Default stock location is missing".into())
        }
        other => Error::Database(other),
    })
}
pub(crate) fn find_stock_batch(
    conn: &Connection,
    company: i64,
    item: i64,
    name: &str,
) -> Result<Option<i64>> {
    let mut stmt =
        conn.prepare("SELECT id, name FROM stock_batches WHERE company_id = ?1 AND item_id = ?2")?;
    let mut found = None;
    for row in stmt.query_map(params![company, item], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, existing) = row?;
        if existing.trim().to_lowercase() == name.trim().to_lowercase() {
            if found.is_some() {
                return Err(Error::Validation("Stock batch name is ambiguous".into()));
            }
            found = Some(id);
        }
    }
    Ok(found)
}
pub(crate) fn check_stock_batch(
    conn: &Connection,
    company: i64,
    item: i64,
    batch: Option<i64>,
) -> Result<()> {
    if let Some(batch) = batch {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM stock_batches WHERE id = ?1 AND company_id = ?2 AND item_id = ?3)", params![batch, company, item], |r| r.get(0))?;
        if !exists {
            return Err(Error::Validation(
                "Batch must belong to the selected stock item and company".into(),
            ));
        }
    }
    Ok(())
}
pub(crate) fn create_stock_batch_on_connection(
    conn: &Connection,
    company: i64,
    item: i64,
    name: &str,
    manufactured_on: Option<&str>,
    expires_on: Option<&str>,
) -> Result<i64> {
    let name = required(name, "Batch name")?;
    let item_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
        params![item, company],
        |r| r.get(0),
    )?;
    if !item_exists {
        return Err(Error::Validation(
            "Stock item not found in the selected company".into(),
        ));
    }
    if find_stock_batch(conn, company, item, name)?.is_some() {
        return Err(Error::Validation(
            "Batch already exists for this item".into(),
        ));
    }
    let manufactured = manufactured_on.map(parse_date).transpose()?;
    let expires = expires_on.map(parse_date).transpose()?;
    if matches!((manufactured, expires), (Some(a), Some(b)) if a > b) {
        return Err(Error::Validation(
            "Expiry date cannot precede manufacture date".into(),
        ));
    }
    let manufactured = manufactured.map(|d| d.format("%Y-%m-%d").to_string());
    let expires = expires.map(|d| d.format("%Y-%m-%d").to_string());
    conn.execute("INSERT INTO stock_batches(company_id, item_id, name, manufactured_on, expires_on) VALUES (?1, ?2, ?3, ?4, ?5)", params![company, item, name, manufactured, expires])?;
    Ok(conn.last_insert_rowid())
}
pub(crate) fn check_quantity_precision(
    conn: &Connection,
    company_id: i64,
    item_id: i64,
    quantity: i64,
) -> Result<()> {
    let places: i64 = conn.query_row("SELECT u.decimal_places FROM stock_items i JOIN stock_units u ON u.id = i.unit_id AND u.company_id = i.company_id WHERE i.id = ?1 AND i.company_id = ?2", params![item_id, company_id], |r| r.get(0)).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Stock item or its unit was not found in the selected company".into()), other => Error::Database(other) })?;
    if !(0..=3).contains(&places) {
        return Err(Error::Validation("Unsupported unit precision".into()));
    }
    let step = 10_i64.pow((3 - places) as u32);
    if quantity % step != 0 {
        return Err(Error::Validation(format!(
            "Quantity must have at most {places} decimal places for this unit"
        )));
    }
    Ok(())
}
fn find_unit(store: &Store, company_id: i64, symbol: &str) -> Result<Option<i64>> {
    let units = store.stock_units(company_id)?;
    let measures = crate::stock_units::saved_measures(&units)?;
    let Some(measure) = measures.get(&crate::stock_units::unit_key(symbol)) else {
        return Ok(None);
    };
    Ok(units
        .iter()
        .find(|unit| unit.symbol == measure.symbol)
        .map(|unit| unit.id))
}

pub(crate) fn create_stock_item_on_connection(
    conn: &Connection,
    company_id: i64,
    name: &str,
    unit_id: i64,
    group_id: i64,
    description: &str,
) -> Result<i64> {
    let name = required(name, "Stock item name")?;
    let unit: String = conn
        .query_row(
            "SELECT COALESCE(stock.symbol, u.symbol) FROM stock_units u
             LEFT JOIN stock_unit_compounds c ON c.unit_id = u.id
             LEFT JOIN stock_units stock ON stock.id = c.stock_unit_id AND stock.company_id = u.company_id
             WHERE u.id = ?1 AND u.company_id = ?2",
            params![unit_id, company_id],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Validation("Unit not found in the selected company".into())
            }
            other => Error::Database(other),
        })?;
    let group_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stock_groups WHERE id = ?1 AND company_id = ?2)",
        params![group_id, company_id],
        |r| r.get(0),
    )?;
    if !group_exists {
        return Err(Error::Validation(
            "Stock group not found in the selected company".into(),
        ));
    }
    let mut stmt = conn.prepare("SELECT name FROM stock_items WHERE company_id = ?1")?;
    for existing in stmt.query_map([company_id], |r| r.get::<_, String>(0))? {
        if existing?.trim().to_lowercase() == name.to_lowercase() {
            return Err(Error::Validation("Stock item already exists".into()));
        }
    }
    drop(stmt);
    conn.execute("INSERT INTO stock_items(company_id, name, unit, unit_id, group_id, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![company_id, name, unit, unit_id, group_id, description.trim()])?;
    Ok(conn.last_insert_rowid())
}
pub(crate) fn invoice_total(conn: &Connection, invoice: i64, tax: i64) -> Result<i128> {
    let mut stmt = conn.prepare(
        "SELECT amount FROM invoice_lines WHERE invoice_id = ?1
         UNION ALL SELECT amount FROM invoice_adjustments WHERE invoice_id = ?1
         UNION ALL SELECT amount FROM invoice_additional_taxes WHERE invoice_id = ?1
         UNION ALL SELECT amount FROM opening_bills WHERE invoice_id = ?1",
    )?;
    let mut total = i128::from(tax);
    for row in stmt.query_map([invoice], |row| row.get::<_, i64>(0))? {
        total += i128::from(row?);
    }
    Ok(total)
}

pub(crate) fn insert_settlement_on_connection(
    conn: &Connection,
    company_id: i64,
    bank_ledger_id: i64,
    date: &str,
    reference: &str,
    allocations: &[BillSettlementAllocation],
    narration: &str,
    advance_voucher: Option<i64>,
    discounts: &[crate::settlement_discounts::SettlementDiscount],
    refund_total: i64,
) -> Result<(i64, Vec<i64>)> {
    let parsed_date = parse_date(date)?;
    let date = parsed_date.format("%Y-%m-%d").to_string();
    let reference = required(reference, "Settlement reference")?;
    if allocations.is_empty() {
        return Err(Error::Validation("Select at least one invoice".into()));
    }
    if ledger_category(conn, company_id, bank_ledger_id)? != Category::Asset {
        return Err(Error::Validation(
            "Select an asset ledger for cash or bank".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut positions = HashMap::new();
    let mut groups = Vec::<(i64, i128, Vec<(i64, i64)>)>::new();
    for allocation in allocations {
        let target = allocation.target;
        if allocation.amount <= 0 || !seen.insert(target) {
            return Err(Error::Validation(
                "Each bill must appear once with a positive allocation".into(),
            ));
        }
        let index = *positions.entry(target.invoice_id).or_insert_with(|| {
            groups.push((target.invoice_id, 0, Vec::new()));
            groups.len() - 1
        });
        let group = &mut groups[index];
        group.1 += i128::from(allocation.amount);
        if let Some(split_id) = target.split_id {
            group.2.push((split_id, allocation.amount));
        }
    }
    let mut common: Option<(InvoiceKind, i64)> = None;
    let mut total = 0_i128;
    for (invoice_id, amount, splits) in &groups {
        let invoice_id = *invoice_id;
        let (kind, party, invoice_date, tax): (String, i64, String, i64) = conn.query_row("SELECT i.kind, i.party_ledger_id, v.date, i.tax_amount FROM invoices i JOIN vouchers v ON v.id = i.voucher_id WHERE i.id = ?1 AND i.company_id = ?2", params![invoice_id, company_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Invoice not found in the selected company".into()), other => Error::Database(other) })?;
        let bills = crate::bill_splits::read(conn, invoice_id, None)?;
        if !bills.is_empty()
            && seen.contains(&BillSettlementTarget {
                invoice_id,
                split_id: None,
            })
        {
            return Err(Error::Validation(
                "Select the individual bills to settle on this invoice".into(),
            ));
        }
        for (id, amount) in splits {
            let bill = bills.iter().find(|bill| bill.id == *id).ok_or_else(|| {
                Error::Validation("The selected bill is unavailable on this invoice".into())
            })?;
            if i128::from(*amount) > bill.outstanding() {
                return Err(Error::Validation(format!(
                    "The allocation exceeds the outstanding amount for bill {}",
                    bill.reference
                )));
            }
        }
        let kind = InvoiceKind::parse(&kind)?;
        if parsed_date < parse_date(&invoice_date)? {
            return Err(Error::Validation(
                "Settlement date must not precede any invoice date".into(),
            ));
        }
        if crate::invoice_reversals::for_invoice(conn, company_id, invoice_id)?.is_some() {
            return Err(Error::Validation(
                "A reversed invoice cannot accept settlements".into(),
            ));
        }
        if bank_ledger_id == party {
            return Err(Error::Validation(
                "Cash or bank ledger must differ from the party".into(),
            ));
        }
        if common.is_some_and(|entry| entry != (kind, party)) {
            return Err(Error::Validation(
                "All invoices must have the same party and type".into(),
            ));
        }
        common = Some((kind, party));
        let total_on_invoice = invoice_total(conn, invoice_id, tax)?;
        let advances =
            crate::advances::invoice_allocation_state(conn, company_id, invoice_id, None)?;
        let credits = crate::invoice_credits::state(conn, company_id, invoice_id, None)?;
        if advances
            .latest_release
            .max(credits.latest_reversal)
            .is_some_and(|date| parsed_date < date)
        {
            return Err(Error::Validation(
                "Settlement date cannot precede an advance release or note reversal on the invoice"
                    .into(),
            ));
        }
        let mut paid = 0_i128;
        let mut previous = conn.prepare("SELECT s.amount, v.reference, rv.date FROM invoice_settlements s JOIN vouchers v ON v.id = s.voucher_id LEFT JOIN settlement_reversals r ON r.company_id = s.company_id AND r.settlement_voucher_id = s.voucher_id LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id WHERE s.invoice_id = ?1 AND s.company_id = ?2")?;
        for row in previous.query_map(params![invoice_id, company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (value, old_reference, reversal_date) = row?;
            if let Some(reversal_date) = reversal_date {
                if parsed_date < parse_date(&reversal_date)? {
                    return Err(Error::Validation(
                        "Settlement date must not precede an existing reversal on the invoice"
                            .into(),
                    ));
                }
            } else {
                paid += i128::from(value);
            }
            if old_reference.trim().to_lowercase() == reference.to_lowercase() {
                return Err(Error::Validation(
                    "Settlement reference already exists for an invoice".into(),
                ));
            }
        }
        drop(previous);
        if *amount > total_on_invoice - paid - advances.applied - credits.applied {
            return Err(Error::Validation(
                "An allocation exceeds its invoice's outstanding amount".into(),
            ));
        }
        total += amount;
    }
    let (kind, party) =
        common.ok_or_else(|| Error::Validation("Select at least one invoice".into()))?;
    let voucher_kind = if kind == InvoiceKind::Sale {
        VoucherKind::Receipt
    } else {
        VoucherKind::Payment
    };
    let advance = advance_voucher
        .map(|voucher| {
            settlement_advance_total(conn, company_id, voucher, kind, party, bank_ledger_id)
        })
        .transpose()?
        .flatten();
    if advance_voucher.is_some() && advance.is_none() {
        return Err(Error::Validation(
            "Advance for this settlement is missing".into(),
        ));
    }
    let postings = crate::settlement_discounts::postings(
        conn,
        company_id,
        kind,
        bank_ledger_id,
        party,
        total,
        i128::from(advance.unwrap_or(0)) + i128::from(refund_total),
        discounts,
    )?;
    validate_voucher(&date, &postings)?;
    check_ledgers(conn, company_id, &postings)?;
    crate::opening::check_no_control_postings(conn, company_id, &postings)?;
    let voucher_id = if let Some(voucher) = advance_voucher {
        let can_attach: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM vouchers v
                WHERE v.id = ?1 AND v.company_id = ?2 AND v.kind = ?3 AND v.date = ?4 AND v.reference = ?5
                AND NOT EXISTS(SELECT 1 FROM invoice_settlements s WHERE s.voucher_id = v.id))",
            params![voucher, company_id, voucher_kind.as_str(), date, reference],
            |row| row.get(0),
        )?;
        if !can_attach {
            return Err(Error::Validation(
                "Advance voucher does not match the settlement".into(),
            ));
        }
        check_settlement_postings(conn, voucher, &postings)?;
        let mut discounts = discounts.to_vec();
        discounts.sort_unstable_by_key(|discount| discount.ledger_id);
        if discounts != crate::settlement_discounts::read(conn, voucher)? {
            return Err(Error::Validation(
                "Advance voucher discounts do not match the settlement".into(),
            ));
        }
        voucher
    } else {
        conn.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, voucher_kind.as_str(), reference, date, narration.trim()])?;
        let voucher = conn.last_insert_rowid();
        write_postings(conn, voucher, &postings)?;
        crate::settlement_discounts::save(conn, voucher, discounts)?;
        voucher
    };
    let mut ids = Vec::with_capacity(groups.len());
    for (invoice_id, amount, splits) in groups {
        let amount = i64::try_from(amount)
            .map_err(|_| Error::Validation("Settlement amount is too large".into()))?;
        conn.execute("INSERT INTO invoice_settlements(company_id, invoice_id, voucher_id, bank_ledger_id, amount) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, invoice_id, voucher_id, bank_ledger_id, amount])?;
        let settlement_id = conn.last_insert_rowid();
        for (split_id, amount) in splits {
            conn.execute(
                "INSERT INTO bill_split_settlements(settlement_id, split_id, invoice_id, amount)
                 VALUES (?1, ?2, ?3, ?4)",
                params![settlement_id, split_id, invoice_id, amount],
            )?;
        }
        ids.push(settlement_id);
    }
    Ok((voucher_id, ids))
}

fn settlement_advance_total(
    conn: &Connection,
    company: i64,
    voucher: i64,
    kind: InvoiceKind,
    party: i64,
    bank: i64,
) -> Result<Option<i64>> {
    let row: Option<(Option<i64>, String, i64, i64, bool)> = conn
        .query_row(
            "SELECT (SELECT SUM(a.amount) FROM party_advances a WHERE a.voucher_id = h.voucher_id),
                h.kind, h.party_ledger_id, h.bank_ledger_id,
                h.reversal_voucher_id IS NOT NULL OR h.opening_original_date IS NOT NULL
         FROM advance_vouchers h WHERE h.company_id = ?1 AND h.voucher_id = ?2",
            params![company, voucher],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((Some(amount), stored_kind, stored_party, stored_bank, false))
            if amount > 0
                && stored_kind == kind.as_str()
                && stored_party == party
                && stored_bank == bank =>
        {
            Ok(Some(amount))
        }
        Some(_) => Err(Error::Validation(
            "Settlement and advances are inconsistent".into(),
        )),
    }
}

fn check_settlement_postings(conn: &Connection, voucher: i64, postings: &[Posting]) -> Result<()> {
    let mut expected: Vec<_> = postings.iter().map(|p| (p.ledger_id, p.amount)).collect();
    expected.sort_unstable();
    let mut stmt = conn.prepare_cached(
        "SELECT ledger_id, amount FROM postings WHERE voucher_id = ?1 ORDER BY ledger_id, amount",
    )?;
    let rows = stmt.query_map([voucher], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if rows.collect::<rusqlite::Result<Vec<(i64, i64)>>>()? != expected {
        return Err(Error::Validation(
            "Settlement postings do not match its allocations".into(),
        ));
    }
    Ok(())
}

pub(crate) fn reverse_settlement_on_connection(
    conn: &Connection,
    company_id: i64,
    settlement_voucher: i64,
    reversal_date: NaiveDate,
    reference: &str,
    reason: &str,
) -> Result<i64> {
    let date = reversal_date.format("%Y-%m-%d").to_string();
    let (original_date, original_kind): (String, String) = conn.query_row(
            "SELECT v.date, v.kind FROM vouchers v WHERE v.id = ?1 AND v.company_id = ?2 AND EXISTS(SELECT 1 FROM invoice_settlements s WHERE s.company_id = v.company_id AND s.voucher_id = v.id)",
            params![settlement_voucher, company_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).map_err(|e| match e { rusqlite::Error::QueryReturnedNoRows => Error::Validation("Settlement voucher not found in the selected company".into()), other => Error::Database(other) })?;
    if reversal_date < parse_date(&original_date)? {
        return Err(Error::Validation(
            "Reversal date must not precede the settlement".into(),
        ));
    }
    let already_reversed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM settlement_reversals WHERE company_id = ?1 AND settlement_voucher_id = ?2)", params![company_id, settlement_voucher], |r| r.get(0))?;
    if already_reversed {
        return Err(Error::Validation(
            "Settlement has already been reversed".into(),
        ));
    }
    let mut allocations = conn.prepare("SELECT s.amount, s.bank_ledger_id, i.party_ledger_id, i.kind, i.company_id FROM invoice_settlements s JOIN invoices i ON i.id = s.invoice_id WHERE s.company_id = ?1 AND s.voucher_id = ?2")?;
    let mut total = 0_i128;
    let mut allocation_count = 0_i64;
    let mut common: Option<(i64, i64, InvoiceKind)> = None;
    for row in allocations.query_map(params![company_id, settlement_voucher], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })? {
        let (amount, bank, party, kind, owner) = row?;
        let kind = InvoiceKind::parse(&kind)?;
        if owner != company_id
            || amount <= 0
            || common.is_some_and(|value| value != (bank, party, kind))
        {
            return Err(Error::Validation(
                "Settlement allocations are inconsistent".into(),
            ));
        }
        common = Some((bank, party, kind));
        total += i128::from(amount);
        allocation_count += 1;
    }
    drop(allocations);
    let stored_count: i64 = conn.query_row(
        "SELECT count(*) FROM invoice_settlements WHERE company_id = ?1 AND voucher_id = ?2",
        params![company_id, settlement_voucher],
        |r| r.get(0),
    )?;
    if allocation_count != stored_count {
        return Err(Error::Validation(
            "Settlement allocations are incomplete".into(),
        ));
    }
    let (bank, party, kind) =
        common.ok_or_else(|| Error::Validation("Settlement has no allocations".into()))?;
    let advance =
        settlement_advance_total(conn, company_id, settlement_voucher, kind, party, bank)?;
    if advance.is_some() {
        crate::advances::check_advance_voucher_reversal(
            conn,
            company_id,
            settlement_voucher,
            reversal_date,
        )?;
    }
    let expected_kind = if kind == InvoiceKind::Sale {
        VoucherKind::Receipt
    } else {
        VoucherKind::Payment
    };
    if original_kind != expected_kind.as_str() {
        return Err(Error::Validation(
            "Settlement voucher type is inconsistent".into(),
        ));
    }
    let discounts = crate::settlement_discounts::read(conn, settlement_voucher)?;
    let refunds = crate::bill_note_refunds::total_for_voucher(conn, settlement_voucher)?;
    let expected_postings = crate::settlement_discounts::postings(
        conn,
        company_id,
        kind,
        bank,
        party,
        total,
        i128::from(advance.unwrap_or(0)) + i128::from(refunds),
        &discounts,
    )?;
    check_settlement_postings(conn, settlement_voucher, &expected_postings)?;
    let postings = crate::reversals::counter_postings(conn, company_id, settlement_voucher, &date)?;
    conn.execute("INSERT INTO vouchers(company_id, kind, reference, date, narration) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, expected_kind.as_str(), reference, date, format!("Reversal of settlement voucher {settlement_voucher}: {reason}")])?;
    let reversal_voucher = conn.last_insert_rowid();
    crate::reversals::write_counter_postings(
        conn,
        company_id,
        settlement_voucher,
        reversal_voucher,
        &postings,
    )?;
    conn.execute("INSERT INTO settlement_reversals(company_id, settlement_voucher_id, reversal_voucher_id, reason) VALUES (?1, ?2, ?3, ?4)", params![company_id, settlement_voucher, reversal_voucher, reason])?;
    if advance.is_some() {
        conn.execute(
            "UPDATE advance_vouchers SET reversal_voucher_id = ?1, reversal_reason = ?2
             WHERE company_id = ?3 AND voucher_id = ?4",
            params![reversal_voucher, reason, company_id, settlement_voucher],
        )?;
    }
    Ok(reversal_voucher)
}

// None for the batch means the unbatched bucket. None for location includes all locations.
pub(crate) fn stock_bucket_quantity(
    conn: &Connection,
    company: i64,
    item: i64,
    batch: Option<i64>,
    location: Option<i64>,
    end: Option<chrono::NaiveDate>,
) -> Result<i128> {
    stock_bucket_quantity_in_scope(conn, company, item, batch, location, false, end)
}

fn stock_bucket_movements(conn: &Connection) -> rusqlite::Result<rusqlite::CachedStatement<'_>> {
    let sql = format!(
        "{STOCK_LOCATION_BRANCH}
         SELECT date, quantity FROM warehouse_stock_movements
         WHERE company_id = ?1 AND item_id = ?4 AND batch_id IS ?5
           AND ((?2 IS NULL AND location_only = 0) OR location_id IN (SELECT id FROM branch))"
    );
    conn.prepare_cached(&sql)
}

fn stock_bucket_quantity_in_scope(
    conn: &Connection,
    company: i64,
    item: i64,
    batch: Option<i64>,
    location: Option<i64>,
    include_descendants: bool,
    end: Option<chrono::NaiveDate>,
) -> Result<i128> {
    let mut stmt = stock_bucket_movements(conn)?;
    let mut total = 0_i128;
    let rows = stmt.query_map(
        params![company, location, include_descendants, item, batch],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )?;
    for row in rows {
        let (date, quantity) = row?;
        let date = parse_date(&date)?;
        if end.is_none_or(|limit| date <= limit) {
            total += i128::from(quantity);
        }
    }
    Ok(total)
}

#[derive(Default)]
pub(crate) struct StockChanges {
    opening: i128,
    later: BTreeMap<NaiveDate, i128>,
}

impl StockChanges {
    pub(crate) fn add(&mut self, from: NaiveDate, date: NaiveDate, quantity: i64) {
        if date <= from {
            self.opening += i128::from(quantity);
        } else {
            *self.later.entry(date).or_default() += i128::from(quantity);
        }
    }

    pub(crate) fn closing_balances(
        self,
        from: NaiveDate,
    ) -> impl Iterator<Item = (NaiveDate, i128)> {
        let mut running = self.opening;
        std::iter::once((from, running)).chain(self.later.into_iter().map(
            move |(date, movement)| {
                running += movement;
                (date, running)
            },
        ))
    }
}

pub(crate) fn stock_bucket_changes(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
    from: NaiveDate,
) -> Result<StockChanges> {
    let mut stmt = stock_bucket_movements(conn)?;
    let mut changes = StockChanges::default();
    let rows = stmt.query_map(params![company, location, false, item, batch], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (date, quantity) = row?;
        changes.add(from, parse_date(&date)?, quantity);
    }
    Ok(changes)
}

pub(crate) fn check_stock_after_decrease(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
) -> Result<()> {
    crate::stock_reservations::check_stock_coverage(conn, company, item, location, batch)?;
    crate::stock_policy::check_bucket(conn, company, item, location, batch)
}

/// Record an adjustment inside the caller's transaction.
pub(crate) fn record_stock_adjustment(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
    date: &str,
    quantity: i64,
    reason: &str,
) -> Result<i64> {
    let id = insert_stock_adjustment(conn, company, item, location, batch, date, quantity, reason)?;
    if quantity < 0 {
        check_stock_after_decrease(conn, company, item, location, batch)?;
    }
    Ok(id)
}

/// In a bulk import, the negative-stock policy checks the final transaction state.
pub(crate) fn insert_stock_adjustment(
    conn: &Connection,
    company: i64,
    item: i64,
    location: i64,
    batch: Option<i64>,
    date: &str,
    quantity: i64,
    reason: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO stock_adjustments(company_id, item_id, date, quantity, reason, location_id, batch_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![company, item, date, quantity, reason, location, batch],
    )?;
    Ok(conn.last_insert_rowid())
}

impl Store {
    pub fn stock_batches(&self, company: i64, item: i64) -> Result<Vec<StockBatch>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
            params![item, company],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Stock item not found in the selected company".into(),
            ));
        }
        let mut stmt = self.conn.prepare("SELECT id, item_id, name, manufactured_on, expires_on FROM stock_batches WHERE company_id = ?1 AND item_id = ?2 ORDER BY name")?;
        let rows = stmt
            .query_map(params![company, item], |r| {
                Ok(StockBatch {
                    id: r.get(0)?,
                    item_id: r.get(1)?,
                    name: r.get(2)?,
                    manufactured_on: r.get(3)?,
                    expires_on: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn create_stock_batch(
        &self,
        company: i64,
        item: i64,
        name: &str,
        manufactured_on: Option<&str>,
        expires_on: Option<&str>,
    ) -> Result<i64> {
        create_stock_batch_on_connection(
            &self.conn,
            company,
            item,
            name,
            manufactured_on,
            expires_on,
        )
    }
    /// Quantity for one batch, optionally in one location, through an inclusive date.
    pub fn stock_batch_quantity(
        &self,
        company: i64,
        item: i64,
        batch: i64,
        location: Option<i64>,
        as_of: Option<&str>,
    ) -> Result<i128> {
        self.stock_batch_quantity_in_scope(company, item, batch, location, false, as_of)
    }
    pub fn stock_batch_quantity_in_scope(
        &self,
        company: i64,
        item: i64,
        batch: i64,
        location: Option<i64>,
        include_descendants: bool,
        as_of: Option<&str>,
    ) -> Result<i128> {
        check_stock_batch(&self.conn, company, item, Some(batch))?;
        if let Some(location) = location {
            check_stock_location(&self.conn, company, location)?;
        }
        let end = as_of.map(parse_date).transpose()?;
        stock_bucket_quantity_in_scope(
            &self.conn,
            company,
            item,
            Some(batch),
            location,
            include_descendants,
            end,
        )
    }
    pub fn stock_locations(&self, company_id: i64) -> Result<Vec<StockLocation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id FROM stock_locations WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockLocation {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parent_id: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn create_stock_location(&self, company_id: i64, name: &str) -> Result<i64> {
        self.create_stock_location_under(company_id, name, None)
    }

    pub fn create_stock_location_under(
        &self,
        company_id: i64,
        name: &str,
        parent_id: Option<i64>,
    ) -> Result<i64> {
        let name = required(name, "Stock location name")?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let locations = self.stock_locations(company_id)?;
        if locations
            .iter()
            .any(|l| l.name.trim().to_lowercase() == name.to_lowercase())
        {
            return Err(Error::Validation("Stock location already exists".into()));
        }
        if parent_id.is_some_and(|id| !locations.iter().any(|location| location.id == id)) {
            return Err(Error::Validation(
                "The parent location must belong to the selected company".into(),
            ));
        }
        tx.execute(
            "INSERT INTO stock_locations(company_id, name, parent_id) VALUES (?1, ?2, ?3)",
            params![company_id, name, parent_id],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }
    pub fn transfer_stock(
        &self,
        company_id: i64,
        item_id: i64,
        from: i64,
        to: i64,
        date: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<i64> {
        self.transfer_stock_batch(company_id, item_id, None, from, to, date, quantity, reason)
    }
    pub fn transfer_stock_batch(
        &self,
        company_id: i64,
        item_id: i64,
        batch_id: Option<i64>,
        from: i64,
        to: i64,
        date: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let reason = required(reason, "Reason")?;
        if quantity <= 0 || from == to {
            return Err(Error::Validation(
                "Choose different locations and a positive quantity".into(),
            ));
        }
        let locations = self.stock_locations(company_id)?;
        if !locations.iter().any(|l| l.id == from) || !locations.iter().any(|l| l.id == to) {
            return Err(Error::Validation(
                "Both locations must belong to the selected company".into(),
            ));
        }
        check_quantity_precision(&self.conn, company_id, item_id, quantity)?;
        check_stock_batch(&self.conn, company_id, item_id, batch_id)?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute("INSERT INTO stock_transfers(company_id, item_id, from_location_id, to_location_id, date, quantity, reason, batch_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![company_id, item_id, from, to, date, quantity, reason, batch_id])?;
        let id = tx.last_insert_rowid();
        check_stock_after_decrease(&tx, company_id, item_id, from, batch_id)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn stock_transfers(&self, company_id: i64) -> Result<Vec<StockTransfer>> {
        let mut stmt = self.conn.prepare("SELECT t.date, i.name, i.unit, f.name, d.name, t.quantity, t.reason, b.name FROM stock_transfers t JOIN stock_items i ON i.id = t.item_id AND i.company_id = t.company_id JOIN stock_locations f ON f.id = t.from_location_id AND f.company_id = t.company_id JOIN stock_locations d ON d.id = t.to_location_id AND d.company_id = t.company_id LEFT JOIN stock_batches b ON b.id = t.batch_id WHERE t.company_id = ?1 ORDER BY t.date DESC, t.id DESC")?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockTransfer {
                    date: r.get(0)?,
                    item: r.get(1)?,
                    unit: r.get(2)?,
                    batch: r.get(7)?,
                    from: r.get(3)?,
                    to: r.get(4)?,
                    quantity: r.get(5)?,
                    reason: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn stock_units(&self, company_id: i64) -> Result<Vec<StockUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id, u.symbol, u.formal_name, u.decimal_places, p.symbol, t.symbol, c.factor
             FROM stock_units u LEFT JOIN stock_unit_compounds c ON c.unit_id = u.id
             LEFT JOIN stock_units p ON p.id = c.primary_unit_id AND p.company_id = u.company_id
             LEFT JOIN stock_units t ON t.id = c.tail_unit_id AND t.company_id = u.company_id
             WHERE u.company_id = ?1 ORDER BY u.symbol",
        )?;
        let mut rows = stmt.query([company_id])?;
        let mut units = Vec::new();
        while let Some(row) = rows.next()? {
            let compound = row
                .get::<_, Option<String>>(6)?
                .map(|factor| -> Result<_> {
                    Ok(crate::stock_units::CompoundUnit {
                        primary: row.get(4)?,
                        tail: row.get(5)?,
                        factor: crate::stock_units::factor(&factor)?,
                    })
                })
                .transpose()?;
            units.push(StockUnit {
                id: row.get(0)?,
                symbol: row.get(1)?,
                formal_name: row.get(2)?,
                decimal_places: row.get(3)?,
                compound,
            });
        }
        Ok(units)
    }
    pub fn create_stock_unit(
        &self,
        company_id: i64,
        symbol: &str,
        formal_name: &str,
        decimal_places: u8,
    ) -> Result<i64> {
        let symbol = required(symbol, "Unit symbol")?;
        if decimal_places > 3 {
            return Err(Error::Validation(
                "Units currently support at most three decimal places".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        if find_unit(self, company_id, symbol)?.is_some() {
            return Err(Error::Validation("Unit already exists".into()));
        }
        tx.execute("INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places) VALUES (?1, ?2, ?3, ?4)", params![company_id, symbol, formal_name.trim(), decimal_places])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }
    pub fn stock_groups(&self, company_id: i64) -> Result<Vec<StockGroup>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id FROM stock_groups WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockGroup {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parent_id: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn create_stock_group(&self, company_id: i64, name: &str, parent_id: i64) -> Result<i64> {
        let name = required(name, "Stock group name")?;
        let groups = self.stock_groups(company_id)?;
        if !groups.iter().any(|g| g.id == parent_id) {
            return Err(Error::Validation(
                "Parent stock group not found in the selected company".into(),
            ));
        }
        if groups
            .iter()
            .any(|g| g.name.trim().to_lowercase() == name.to_lowercase())
        {
            return Err(Error::Validation("Stock group already exists".into()));
        }
        self.conn.execute(
            "INSERT INTO stock_groups(company_id, name, parent_id) VALUES (?1, ?2, ?3)",
            params![company_id, name, parent_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn create_stock_item_in_group(
        &self,
        company_id: i64,
        name: &str,
        unit_id: i64,
        group_id: i64,
    ) -> Result<i64> {
        create_stock_item_on_connection(&self.conn, company_id, name, unit_id, group_id, "")
    }
    pub fn create_stock_item_with_description(
        &self,
        company_id: i64,
        name: &str,
        unit_id: i64,
        group_id: i64,
        description: &str,
    ) -> Result<i64> {
        create_stock_item_on_connection(
            &self.conn,
            company_id,
            name,
            unit_id,
            group_id,
            description,
        )
    }
    pub fn create_stock_item(&self, company_id: i64, name: &str, unit: &str) -> Result<i64> {
        let name = required(name, "Stock item name")?;
        let unit = required(unit, "Unit")?;
        let tx = self.conn.unchecked_transaction()?;
        let unit_id = if let Some(id) = find_unit(self, company_id, unit)? {
            id
        } else {
            tx.execute("INSERT INTO stock_units(company_id, symbol, formal_name, decimal_places) VALUES (?1, ?2, ?2, 3)", params![company_id, unit])?;
            tx.last_insert_rowid()
        };
        let group_id: i64 = tx.query_row("SELECT id FROM stock_groups WHERE company_id = ?1 AND name = 'Primary' AND parent_id IS NULL", [company_id], |r| r.get(0))?;
        let id = create_stock_item_on_connection(&tx, company_id, name, unit_id, group_id, "")?;
        tx.commit()?;
        Ok(id)
    }
    pub fn stock_items(&self, company_id: i64) -> Result<Vec<StockItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, unit, unit_id, group_id, description FROM stock_items WHERE company_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map([company_id], |r| {
                Ok(StockItem {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    unit: r.get(2)?,
                    unit_id: r.get(3)?,
                    group_id: r.get(4)?,
                    description: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Retain the previous name, group and description with each change.
    /// The base unit is deliberately immutable because existing quantities use it.
    pub fn update_stock_item(
        &mut self,
        company_id: i64,
        item_id: i64,
        name: &str,
        group_id: i64,
        description: &str,
    ) -> Result<()> {
        let name = required(name, "Stock item name")?;
        let tx = self.conn.transaction()?;
        let current: (String, i64, String) = tx.query_row(
            "SELECT name, group_id, description FROM stock_items WHERE id = ?1 AND company_id = ?2",
            params![item_id, company_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::Validation("Stock item not found in the selected company".into()),
            other => Error::Database(other),
        })?;
        let group_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_groups WHERE id = ?1 AND company_id = ?2)",
            params![group_id, company_id],
            |r| r.get(0),
        )?;
        if !group_exists {
            return Err(Error::Validation(
                "Stock group not found in the selected company".into(),
            ));
        }
        let mut stmt =
            tx.prepare("SELECT name FROM stock_items WHERE company_id = ?1 AND id != ?2")?;
        for existing in stmt.query_map(params![company_id, item_id], |r| r.get::<_, String>(0))? {
            if existing?.trim().to_lowercase() == name.to_lowercase() {
                return Err(Error::Validation("Stock item already exists".into()));
            }
        }
        drop(stmt);
        if current.0 != name || current.1 != group_id || current.2 != description.trim() {
            tx.execute("INSERT INTO stock_item_revisions(company_id, item_id, name, group_id, description) VALUES (?1, ?2, ?3, ?4, ?5)", params![company_id, item_id, current.0, current.1, current.2])?;
            tx.execute("UPDATE stock_items SET name = ?1, group_id = ?2, description = ?3 WHERE id = ?4 AND company_id = ?5", params![name, group_id, description.trim(), item_id, company_id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn stock_item_revisions(
        &self,
        company_id: i64,
        item_id: i64,
    ) -> Result<Vec<StockItemRevision>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
            params![item_id, company_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Stock item not found in the selected company".into(),
            ));
        }
        let mut stmt = self.conn.prepare("SELECT r.changed_at, r.name, g.name, r.description FROM stock_item_revisions r JOIN stock_groups g ON g.id = r.group_id AND g.company_id = r.company_id WHERE r.company_id = ?1 AND r.item_id = ?2 ORDER BY r.id DESC")?;
        let rows = stmt
            .query_map(params![company_id, item_id], |r| {
                Ok(StockItemRevision {
                    changed_at: r.get(0)?,
                    name: r.get(1)?,
                    group: r.get(2)?,
                    description: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Quantity-only adjustment. It does not create an accounting entry.
    pub fn adjust_stock(
        &self,
        company_id: i64,
        item_id: i64,
        date: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<i64> {
        let location = main_stock_location(&self.conn, company_id)?;
        self.adjust_stock_at(company_id, item_id, location, date, quantity, reason)
    }
    pub fn adjust_stock_at(
        &self,
        company_id: i64,
        item_id: i64,
        location_id: i64,
        date: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<i64> {
        self.adjust_stock_batch(
            company_id,
            item_id,
            location_id,
            None,
            date,
            quantity,
            reason,
        )
    }
    pub fn adjust_stock_batch(
        &self,
        company_id: i64,
        item_id: i64,
        location_id: i64,
        batch_id: Option<i64>,
        date: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?.format("%Y-%m-%d").to_string();
        let reason = required(reason, "Reason")?;
        if quantity == 0 {
            return Err(Error::Validation("Adjustment must be non-zero".into()));
        }
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stock_items WHERE id = ?1 AND company_id = ?2)",
            params![item_id, company_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Validation(
                "Stock item not found in the selected company".into(),
            ));
        }
        check_quantity_precision(&self.conn, company_id, item_id, quantity)?;
        check_stock_batch(&self.conn, company_id, item_id, batch_id)?;
        if !self
            .stock_locations(company_id)?
            .iter()
            .any(|location| location.id == location_id)
        {
            return Err(Error::Validation(
                "Stock location not found in the selected company".into(),
            ));
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let id = record_stock_adjustment(
            &tx,
            company_id,
            item_id,
            location_id,
            batch_id,
            &date,
            quantity,
            reason,
        )?;
        tx.commit()?;
        Ok(id)
    }
    pub fn stock_location_balances(
        &self,
        company_id: i64,
        location_id: i64,
        as_of: Option<&str>,
    ) -> Result<Vec<StockBalance>> {
        self.location_stock_quantities(company_id, location_id, false, as_of)
    }

    /// Return direct quantities at a location or the sum across its branch.
    pub fn location_stock_quantities(
        &self,
        company_id: i64,
        location_id: i64,
        include_descendants: bool,
        as_of: Option<&str>,
    ) -> Result<Vec<StockBalance>> {
        check_stock_location(&self.conn, company_id, location_id)?;
        let end = as_of.map(parse_date).transpose()?;
        let mut sums = HashMap::<i64, i128>::new();
        let mut stmt = self.conn.prepare(&format!(
            "{STOCK_LOCATION_BRANCH}
             SELECT item_id, date, quantity FROM warehouse_stock_movements
             WHERE company_id = ?1 AND location_id IN (SELECT id FROM branch)",
        ))?;
        let movements = stmt.query_map(
            params![company_id, location_id, include_descendants],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        for movement in movements {
            let (item, date, quantity) = movement?;
            let date = parse_date(&date)?;
            if end.is_none_or(|limit| date <= limit) {
                *sums.entry(item).or_default() += i128::from(quantity);
            }
        }

        Ok(self
            .stock_items(company_id)?
            .into_iter()
            .map(|item| StockBalance {
                quantity: sums.get(&item.id).copied().unwrap_or_default(),
                item,
            })
            .collect())
    }

    pub fn stock_balances(
        &self,
        company_id: i64,
        as_of: Option<&str>,
    ) -> Result<Vec<StockBalance>> {
        let end = as_of.map(parse_date).transpose()?;
        let items = self.stock_items(company_id)?;
        let mut sums = HashMap::<i64, i128>::new();
        let mut adjustments = self.conn.prepare(
            "SELECT item_id, date, quantity FROM stock_adjustments WHERE company_id = ?1",
        )?;
        let rows = adjustments.query_map([company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, date, quantity) = row?;
            let date = parse_date(&date)?;
            if end.is_none_or(|v| date <= v) {
                *sums.entry(id).or_default() += i128::from(quantity);
            }
        }
        let mut invoices = self.conn.prepare("SELECT l.item_id, v.date, i.kind, l.quantity FROM physical_invoice_lines l JOIN invoices i ON i.id = l.invoice_id JOIN vouchers v ON v.id = i.voucher_id WHERE i.company_id = ?1")?;
        let rows = invoices.query_map([company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (id, date, kind, quantity) = row?;
            let date = parse_date(&date)?;
            if end.is_some_and(|v| date > v) {
                continue;
            }
            let sign = if InvoiceKind::parse(&kind)? == InvoiceKind::Purchase {
                1
            } else {
                -1
            };
            *sums.entry(id).or_default() += sign * i128::from(quantity);
        }
        Ok(items
            .into_iter()
            .map(|item| StockBalance {
                quantity: *sums.get(&item.id).unwrap_or(&0),
                item,
            })
            .collect())
    }
    /// Quantity movements ordered by date; a date range includes both boundaries.
    pub fn stock_register(
        &self,
        company_id: i64,
        item_id: i64,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<StockRegister> {
        self.stock_register_at(company_id, item_id, None, start, end)
    }
    pub fn stock_register_at(
        &self,
        company_id: i64,
        item_id: i64,
        location_id: Option<i64>,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<StockRegister> {
        self.stock_register_in_scope(company_id, item_id, location_id, false, start, end)
    }

    pub fn stock_register_in_scope(
        &self,
        company_id: i64,
        item_id: i64,
        location_id: Option<i64>,
        include_descendants: bool,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<StockRegister> {
        if let Some(location) = location_id {
            check_stock_location(&self.conn, company_id, location)?;
        }
        let start = start.map(parse_date).transpose()?;
        let end = end.map(parse_date).transpose()?;
        if matches!((start, end), (Some(a), Some(b)) if a > b) {
            return Err(Error::Validation(
                "The start date must not be after the end date".into(),
            ));
        }
        let item = self
            .stock_items(company_id)?
            .into_iter()
            .find(|i| i.id == item_id)
            .ok_or_else(|| {
                Error::Validation("Stock item not found in the selected company".into())
            })?;
        let mut raw = Vec::<(chrono::NaiveDate, u8, i64, String, i64)>::new();
        let mut stmt = self.conn.prepare(&format!(
            "{STOCK_LOCATION_BRANCH}
             SELECT a.id, a.date, a.quantity, a.reason, loc.name, b.name FROM stock_adjustments a
             JOIN stock_locations loc ON loc.id = a.location_id AND loc.company_id = a.company_id
             LEFT JOIN stock_batches b ON b.id = a.batch_id
             WHERE a.company_id = ?1 AND a.item_id = ?4
               AND (?2 IS NULL OR a.location_id IN (SELECT id FROM branch))"
        ))?;
        let scope = params![company_id, location_id, include_descendants, item_id];
        let rows = stmt.query_map(scope, |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (id, date, qty, reason, location, batch) = row?;
            raw.push((
                parse_date(&date)?,
                0,
                id,
                format!(
                    "Adjustment at {location}, batch {}: {reason}",
                    batch.as_deref().unwrap_or("unbatched")
                ),
                qty,
            ));
        }
        let mut stmt = self.conn.prepare(&format!(
            "{STOCK_LOCATION_BRANCH}
             SELECT a.id, v.date, i.kind, v.reference, a.quantity, loc.name, b.name
             FROM invoice_line_allocations a JOIN physical_invoice_lines l ON l.id = a.line_id
             JOIN invoices i ON i.id = l.invoice_id JOIN vouchers v ON v.id = i.voucher_id
             JOIN stock_locations loc ON loc.id = a.location_id AND loc.company_id = i.company_id
             LEFT JOIN stock_batches b ON b.id = a.batch_id
             WHERE i.company_id = ?1 AND l.item_id = ?4
               AND (?2 IS NULL OR a.location_id IN (SELECT id FROM branch))"
        ))?;
        let rows = stmt.query_map(scope, |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (id, date, kind, reference, qty, location, batch) = row?;
            let kind = InvoiceKind::parse(&kind)?;
            raw.push((
                parse_date(&date)?,
                1,
                id,
                format!(
                    "{}: {} at {}, batch {}",
                    kind.as_str(),
                    reference,
                    location,
                    batch.as_deref().unwrap_or("unbatched")
                ),
                if kind == InvoiceKind::Purchase {
                    qty
                } else {
                    -qty
                },
            ));
        }
        if location_id.is_some() {
            let mut stmt = self.conn.prepare(&format!(
                "{STOCK_LOCATION_BRANCH}
                 SELECT t.id, t.date, t.quantity, f.name, d.name, t.reason, b.name,
                        t.kind, t.reference
                 FROM stock_transfer_location_movements t
                 JOIN stock_locations f ON f.id = t.from_location_id AND f.company_id = t.company_id
                 JOIN stock_locations d ON d.id = t.to_location_id AND d.company_id = t.company_id
                 LEFT JOIN stock_batches b ON b.id = t.batch_id
                 WHERE t.company_id = ?1 AND t.item_id = ?4
                   AND t.location_id IN (SELECT id FROM branch)
                   AND (t.kind != 'Transfer'
                     OR t.from_location_id NOT IN (SELECT id FROM branch)
                     OR t.to_location_id NOT IN (SELECT id FROM branch))
                 ORDER BY t.kind, t.id"
            ))?;
            for row in stmt.query_map(scope, |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            })? {
                let (id, date, quantity, from, to, reason, batch, kind, reference) = row?;
                let route = match kind.as_str() {
                    "Receipt" => format!("Receipt {} at {to}", reference.unwrap_or_default()),
                    "Return to source" => {
                        format!("Return {} to {from}", reference.unwrap_or_default())
                    }
                    "Dispatch" => format!(
                        "Dispatch {} from {from} to {to}",
                        reference.unwrap_or_default()
                    ),
                    _ => format!("Transfer {from} to {to}"),
                };
                let priority = match kind.as_str() {
                    "Dispatch" => 3,
                    "Receipt" | "Return to source" => 4,
                    _ => 2,
                };
                raw.push((
                    parse_date(&date)?,
                    priority,
                    id,
                    format!(
                        "{route} · batch {} · {reason}",
                        batch.as_deref().unwrap_or("unbatched")
                    ),
                    quantity,
                ));
            }
        }
        raw.sort_by_key(|(date, kind, id, _, _)| (*date, *kind, *id));
        let mut opening = 0_i128;
        let mut running = 0_i128;
        let mut movements = Vec::new();
        for (date, _, _, description, quantity) in raw {
            if end.is_some_and(|v| date > v) {
                continue;
            }
            running += i128::from(quantity);
            if start.is_some_and(|v| date < v) {
                opening = running;
            } else {
                movements.push(StockMovement {
                    date: date.format("%Y-%m-%d").to_string(),
                    description,
                    quantity,
                    running_quantity: running,
                });
            }
        }
        Ok(StockRegister {
            item,
            opening_quantity: opening,
            movements,
            closing_quantity: running,
        })
    }
    /// Atomically creates the invoice, stock movements and balanced voucher.
    pub fn create_invoice(&mut self, company_id: i64, draft: &InvoiceDraft) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let (invoice_id, _) =
            insert_direct_invoice(&tx, company_id, draft, StockInvoicePosting::Direct)?;
        tx.commit()?;
        Ok(invoice_id)
    }
    /// Return a complete editable draft, scoped to the company.
    pub fn invoice_draft(&self, company_id: i64, invoice_id: i64) -> Result<InvoiceDraft> {
        crate::opening_bills::ensure_trade_invoice(&self.conn, company_id, invoice_id)?;
        let (kind, date, reference, narration, party_ledger_id, trade_ledger_id,
            tax_ledger_id, tax_amount, due_date, credit_days, bill_reference, entry_mode, external_reference) = self.conn.query_row(
            "SELECT i.kind, v.date, v.reference, v.narration, i.party_ledger_id, i.trade_ledger_id,
                    i.tax_ledger_id, i.tax_amount, i.due_date, i.credit_days, i.bill_reference, i.entry_mode,
                    i.external_reference, i.external_reference_date
             FROM invoices i JOIN vouchers v ON v.id = i.voucher_id
             WHERE i.id = ?1 AND i.company_id = ?2",
            params![invoice_id, company_id],
            |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get(6)?, r.get(7)?, r.get(8)?, r.get::<_, Option<u32>>(9)?, r.get(10)?, r.get::<_, String>(11)?,
                    InvoiceExternalReference { number: r.get(12)?, date: r.get(13)? })),
        ).map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Validation("Invoice not found in the selected company".into())
            }
            other => Error::Database(other),
        })?;
        let mut stmt = self.conn.prepare("SELECT id, item_id, quantity, unit_price, location_id, batch_id, trade_ledger_id, discount_kind, discount_value, amount, discount_amount, rate_basis_quantity, rate_basis_unit, billed_quantity FROM invoice_lines WHERE invoice_id = ?1 ORDER BY id")?;
        let raw_lines = stmt
            .query_map([invoice_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, i64>(10)?,
                    read_invoice_rate_basis(r, 11, 12)?,
                    r.get::<_, Option<i64>>(13)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut lines = Vec::with_capacity(raw_lines.len());
        for (
            id,
            item_id,
            quantity,
            unit_price,
            location_id,
            batch_id,
            trade_ledger_id,
            discount_kind,
            discount_value,
            amount,
            discount_amount,
            rate_basis,
            billed_quantity,
        ) in raw_lines
        {
            let allocations = read_line_allocations(&self.conn, id, false)?;
            check_line_allocations(
                &allocations,
                quantity,
                billed_quantity.unwrap_or(quantity),
                location_id,
                batch_id,
            )?;
            let discount = parse_discount(&discount_kind, &discount_value)?;
            if discounted_line_amount_with_basis(
                billed_quantity.unwrap_or(quantity),
                unit_price,
                &discount,
                rate_basis.as_ref(),
            )? != (amount, discount_amount)
            {
                return Err(Error::Validation(
                    "Invoice discount calculation is inconsistent".into(),
                ));
            }
            lines.push(InvoiceLine {
                item_id,
                quantity,
                unit_price,
                rate_basis,
                billed_quantity,
                location_id,
                batch_id,
                trade_ledger_id,
                extra_allocations: allocations.into_iter().skip(1).collect(),
                discount,
            });
        }
        let mut stmt = self.conn.prepare("SELECT ledger_id, amount, description FROM invoice_adjustments WHERE invoice_id = ?1 ORDER BY id")?;
        let adjustments = stmt
            .query_map([invoice_id], |r| {
                Ok(InvoiceAdjustment {
                    ledger_id: r.get(0)?,
                    amount: r.get(1)?,
                    description: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(InvoiceDraft {
            kind: InvoiceKind::parse(&kind)?,
            entry_mode: InvoiceEntryMode::parse(&entry_mode)?,
            date,
            due_date: if credit_days.is_some() {
                None
            } else {
                due_date
            },
            credit_days,
            reference,
            external_reference,
            bill_reference,
            bill_splits: crate::bill_splits::read(&self.conn, invoice_id, None)?
                .into_iter()
                .map(|bill| BillSplitDraft {
                    reference: bill.reference,
                    amount: bill.amount,
                    due_date: if bill.credit_days.is_some() {
                        None
                    } else {
                        bill.due_date
                    },
                    credit_days: bill.credit_days,
                })
                .collect(),
            narration,
            party_ledger_id,
            trade_ledger_id,
            tax_ledger_id,
            tax_amount,
            additional_taxes: crate::taxes::additional_lines(&self.conn, invoice_id)?,
            tax_calculation: crate::tax_profiles::calculation_detail(
                &self.conn, invoice_id, false,
            )?
            .map(|row| row.calculation),
            lines,
            adjustments,
        })
    }
    /// Atomically replace an unsettled invoice, preserving both prior versions.
    pub fn update_invoice(
        &mut self,
        company_id: i64,
        invoice_id: i64,
        draft: &InvoiceDraft,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        crate::opening_bills::ensure_trade_invoice(&tx, company_id, invoice_id)?;
        let voucher_id: i64 = tx
            .query_row(
                "SELECT voucher_id FROM invoices WHERE id = ?1 AND company_id = ?2",
                params![invoice_id, company_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::Validation("Invoice not found in the selected company".into())
                }
                other => Error::Database(other),
            })?;
        if crate::invoice_reversals::for_invoice(&tx, company_id, invoice_id)?.is_some() {
            return Err(Error::Validation(
                "A reversed invoice cannot be edited".into(),
            ));
        }
        let settled: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM invoice_settlements WHERE company_id = ?1 AND invoice_id = ?2)", params![company_id, invoice_id], |r| r.get(0))?;
        if settled {
            return Err(Error::Validation(
                "An invoice with receipts or payments cannot yet be edited".into(),
            ));
        }
        if crate::orders::linked_order(&tx, invoice_id)?.is_some() {
            return Err(Error::Validation(
                "An invoice linked to an order cannot yet be edited".into(),
            ));
        }
        if crate::stock_notes::linked_note(&tx, invoice_id)?.is_some() {
            return Err(Error::Validation(
                "An invoice linked to a stock note cannot be edited. Reverse it and create a replacement.".into(),
            ));
        }
        if crate::advances::invoice_allocation_state(&tx, company_id, invoice_id, None)?.has_history
        {
            return Err(Error::Validation(
                "An invoice with advance allocation history cannot yet be edited".into(),
            ));
        }
        if crate::invoice_credits::state(&tx, company_id, invoice_id, None)?.has_history {
            return Err(Error::Validation(
                "An invoice with credit or debit note history cannot be edited".into(),
            ));
        }
        crate::costs::ensure_unallocated(&tx, voucher_id)?;
        crate::banking::ensure_no_bank_clearance(&tx, company_id, voucher_id)?;
        let prepared = prepare_invoice(&tx, company_id, draft, Some(invoice_id))?;
        let mut affected_buckets: HashSet<_> = invoice_stock_allocations(&tx, invoice_id)?
            .into_iter()
            .map(|(item, location, batch, _)| (item, location, batch))
            .collect();
        let saved = tx.execute("INSERT INTO voucher_revisions(voucher_id, company_id, date, narration, kind, reference) SELECT id, company_id, date, narration, kind, reference FROM vouchers WHERE id = ?1 AND company_id = ?2", params![voucher_id, company_id])?;
        if saved != 1 {
            return Err(Error::Validation("Associated voucher not found".into()));
        }
        let voucher_revision = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO revision_postings(revision_id, ledger_id, amount, is_party)
             SELECT ?1, ledger_id, amount, is_party FROM postings WHERE voucher_id = ?2 ORDER BY id",
            params![voucher_revision, voucher_id],
        )?;
        tx.execute(
            "INSERT INTO invoice_revisions(company_id, invoice_id, voucher_revision_id, kind, party_ledger_id,
                 trade_ledger_id, tax_ledger_id, tax_amount, due_date, credit_days, bill_reference, entry_mode,
                 external_reference, external_reference_date)
             SELECT company_id, id, ?1, kind, party_ledger_id, trade_ledger_id, tax_ledger_id,
                    tax_amount, due_date, credit_days, bill_reference, entry_mode,
                    external_reference, external_reference_date
             FROM invoices WHERE id = ?2 AND company_id = ?3",
            params![voucher_revision, invoice_id, company_id],
        )?;
        let revision_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO invoice_bill_split_revisions(revision_id, reference, amount, due_date, credit_days)
             SELECT ?1, reference, amount, due_date, credit_days FROM invoice_bill_splits
             WHERE invoice_id = ?2 ORDER BY id",
            params![revision_id, invoice_id],
        )?;
        tx.execute(
            "INSERT INTO invoice_revision_tax_calculations(revision_id, version_id, taxable_amount, whole_invoice)
             SELECT ?1, version_id, taxable_amount, whole_invoice FROM invoice_tax_calculations WHERE invoice_id = ?2",
            params![revision_id, invoice_id],
        )?;
        tx.execute("INSERT INTO invoice_revision_additional_taxes(revision_id, ledger_id, amount, source_rates)
                    SELECT ?1, ledger_id, amount, source_rates FROM invoice_additional_taxes WHERE invoice_id = ?2 ORDER BY id",
                   params![revision_id, invoice_id])?;
        copy_invoice_line_revisions(&tx, invoice_id, revision_id)?;
        tx.execute("INSERT INTO invoice_revision_adjustments(revision_id, ledger_id, amount, description) SELECT ?1, ledger_id, amount, description FROM invoice_adjustments WHERE invoice_id = ?2 ORDER BY id", params![revision_id, invoice_id])?;
        tx.execute("UPDATE vouchers SET kind = ?1, reference = ?2, date = ?3, narration = ?4 WHERE id = ?5 AND company_id = ?6", params![draft.kind.voucher_kind().as_str(), prepared.reference, prepared.date, draft.narration.trim(), voucher_id, company_id])?;
        tx.execute(
            "UPDATE invoices SET kind = ?1, party_ledger_id = ?2, trade_ledger_id = ?3,
                 tax_ledger_id = ?4, tax_amount = ?5, due_date = ?6, credit_days = ?7,
                 bill_reference = ?8, entry_mode = ?9, external_reference = ?10, external_reference_date = ?11
             WHERE id = ?12 AND company_id = ?13",
            params![
                draft.kind.as_str(),
                draft.party_ledger_id,
                draft.trade_ledger_id,
                draft.tax_ledger_id,
                draft.tax_amount,
                prepared.due_date,
                prepared.credit_days,
                prepared.bill_reference,
                draft.entry_mode.as_str(),
                prepared.external_reference.number,
                prepared.external_reference.date,
                invoice_id,
                company_id
            ],
        )?;
        tx.execute("DELETE FROM postings WHERE voucher_id = ?1", [voucher_id])?;
        tx.execute(
            "DELETE FROM invoice_bill_splits WHERE invoice_id = ?1",
            [invoice_id],
        )?;
        crate::bill_splits::write(&tx, invoice_id, &prepared.bill_splits)?;
        tx.execute("DELETE FROM invoice_line_allocations WHERE line_id IN (SELECT id FROM invoice_lines WHERE invoice_id = ?1)", [invoice_id])?;
        tx.execute(
            "DELETE FROM invoice_lines WHERE invoice_id = ?1",
            [invoice_id],
        )?;
        tx.execute(
            "DELETE FROM invoice_adjustments WHERE invoice_id = ?1",
            [invoice_id],
        )?;
        write_invoice_postings(&tx, voucher_id, draft, &prepared.postings)?;
        write_invoice_lines(
            &tx,
            invoice_id,
            &draft.lines,
            &prepared.amounts,
            &prepared.discounts,
        )?;
        write_invoice_adjustments(&tx, invoice_id, &draft.adjustments)?;
        tx.execute(
            "DELETE FROM invoice_additional_taxes WHERE invoice_id = ?1",
            [invoice_id],
        )?;
        crate::taxes::write_additional(&tx, invoice_id, &draft.additional_taxes)?;
        tx.execute(
            "DELETE FROM invoice_tax_calculations WHERE invoice_id = ?1",
            [invoice_id],
        )?;
        crate::tax_profiles::write_calculation(&tx, invoice_id, draft.tax_calculation)?;
        affected_buckets.extend(
            invoice_stock_allocations(&tx, invoice_id)?
                .into_iter()
                .map(|(item, location, batch, _)| (item, location, batch)),
        );
        for (item, location, batch) in affected_buckets {
            check_stock_after_decrease(&tx, company_id, item, location, batch)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn is_invoice_voucher(&self, company_id: i64, voucher_id: i64) -> Result<bool> {
        if crate::invoice_credits::is_linked_voucher(&self.conn, company_id, voucher_id)? {
            return Ok(true);
        }
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM invoices WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM invoice_settlements WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM settlement_reversals WHERE company_id = ?1 AND reversal_voucher_id = ?2 UNION ALL SELECT 1 FROM invoice_reversals WHERE company_id = ?1 AND reversal_voucher_id = ?2 UNION ALL SELECT 1 FROM advance_vouchers WHERE company_id = ?1 AND ?2 IN (voucher_id, reversal_voucher_id) UNION ALL SELECT 1 FROM payroll_accruals WHERE company_id = ?1 AND ?2 IN (voucher_id, reversal_voucher_id) UNION ALL SELECT 1 FROM payroll_payments WHERE company_id = ?1 AND ?2 IN (voucher_id, reversal_voucher_id) UNION ALL SELECT 1 FROM opening_balances WHERE company_id = ?1 AND voucher_id = ?2 UNION ALL SELECT 1 FROM opening_balance_clears WHERE company_id = ?1 AND (original_voucher_id = ?2 OR clearing_voucher_id = ?2))",
            params![company_id, voucher_id],
            |r| r.get(0),
        )?)
    }
    pub fn invoice_revisions(
        &self,
        company_id: i64,
        invoice_id: i64,
    ) -> Result<Vec<InvoiceRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.changed_at, r.kind, v.date, v.reference, v.narration, p.name, t.name,
                    x.name, r.tax_amount, r.due_date, r.credit_days, COALESCE(r.bill_reference, v.reference), r.entry_mode,
                    r.external_reference, r.external_reference_date
             FROM invoice_revisions r JOIN voucher_revisions v ON v.id = r.voucher_revision_id
             JOIN ledgers p ON p.id = r.party_ledger_id JOIN ledgers t ON t.id = r.trade_ledger_id
             LEFT JOIN ledgers x ON x.id = r.tax_ledger_id
             WHERE r.company_id = ?1 AND r.invoice_id = ?2 ORDER BY r.id DESC",
        )?;
        let rows = stmt
            .query_map(params![company_id, invoice_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<u32>>(11)?,
                    r.get::<_, String>(12)?,
                    r.get::<_, String>(13)?,
                    InvoiceExternalReference {
                        number: r.get(14)?,
                        date: r.get(15)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut result = Vec::new();
        for (
            id,
            changed_at,
            kind,
            date,
            reference,
            narration,
            party,
            trade,
            tax_ledger,
            tax_amount,
            due_date,
            credit_days,
            bill_reference,
            entry_mode,
            external_reference,
        ) in rows
        {
            let mut items = self.conn.prepare("SELECT s.name, s.unit, l.quantity, l.unit_price, l.amount, loc.name, b.name, trade.name, l.id FROM invoice_revision_lines l JOIN invoice_revisions r ON r.id = l.revision_id JOIN ledgers trade ON trade.id = COALESCE(l.trade_ledger_id, r.trade_ledger_id) JOIN stock_items s ON s.id = l.item_id JOIN stock_locations loc ON loc.id = l.location_id LEFT JOIN stock_batches b ON b.id = l.batch_id WHERE l.revision_id = ?1 ORDER BY l.id")?;
            let raw_lines = items
                .query_map([id], |r| {
                    Ok((
                        r.get::<_, i64>(8)?,
                        InvoiceDetailLine {
                            item: r.get(0)?,
                            unit: r.get(1)?,
                            location: r.get(5)?,
                            batch: r.get(6)?,
                            trade_ledger: r.get(7)?,
                            quantity: r.get(2)?,
                            unit_price: r.get(3)?,
                            amount: r.get(4)?,
                            allocations: Vec::new(),
                            discount: InvoiceDiscount::None,
                            discount_amount: 0,
                            billed_quantity: 0,
                            rate_basis: None,
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut lines = Vec::with_capacity(raw_lines.len());
            for (line_id, line) in raw_lines {
                lines.push(attach_allocation_details(&self.conn, line_id, true, line)?);
            }
            let taxes =
                crate::taxes::tax_details(&self.conn, id, true, tax_ledger.as_deref(), tax_amount)?;
            let total_tax = crate::taxes::checked_total(taxes.iter().map(|tax| tax.amount))?;
            result.push(InvoiceRevision {
                changed_at,
                date,
                due_date,
                credit_days,
                kind: InvoiceKind::parse(&kind)?,
                entry_mode: InvoiceEntryMode::parse(&entry_mode)?,
                reference,
                external_reference,
                bill_reference,
                bill_splits: crate::bill_splits::revisions(&self.conn, id)?,
                narration,
                party,
                trade,
                tax_ledger,
                tax_amount: total_tax,
                taxes,
                tax_calculation: crate::tax_profiles::calculation_detail(&self.conn, id, true)?,
                lines,
                adjustments: read_invoice_adjustments(&self.conn, id, true)?,
            });
        }
        Ok(result)
    }
    pub fn invoices(&self, company_id: i64) -> Result<Vec<InvoiceSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.voucher_id, i.kind, v.date, v.reference, l.name, i.tax_amount, i.due_date,
                    i.credit_days, i.party_ledger_id, x.name, COALESCE(i.bill_reference, v.reference), i.entry_mode,
                    i.external_reference, i.external_reference_date
             FROM invoices i JOIN vouchers v ON v.id = i.voucher_id JOIN ledgers l ON l.id = i.party_ledger_id
             LEFT JOIN ledgers x ON x.id = i.tax_ledger_id WHERE i.company_id = ?1 ORDER BY v.date DESC, i.id DESC",
        )?;
        let rows = stmt.query_map([company_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<u32>>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
                InvoiceExternalReference {
                    number: r.get(13)?,
                    date: r.get(14)?,
                },
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (
                id,
                voucher_id,
                kind,
                date,
                reference,
                party,
                tax,
                due_date,
                credit_days,
                party_ledger_id,
                primary_tax_name,
                bill_reference,
                entry_mode,
                external_reference,
            ) = row?;
            let mut amounts = self.conn.prepare("SELECT s.name, s.unit, l.quantity, l.unit_price, l.amount, loc.name, b.name, trade.name, l.id FROM invoice_lines l JOIN invoices i ON i.id = l.invoice_id JOIN ledgers trade ON trade.id = COALESCE(l.trade_ledger_id, i.trade_ledger_id) JOIN stock_items s ON s.id = l.item_id JOIN stock_locations loc ON loc.id = l.location_id LEFT JOIN stock_batches b ON b.id = l.batch_id WHERE l.invoice_id = ?1 ORDER BY l.id")?;
            let line_amounts = amounts.query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(8)?,
                    InvoiceDetailLine {
                        item: r.get(0)?,
                        unit: r.get(1)?,
                        location: r.get(5)?,
                        batch: r.get(6)?,
                        trade_ledger: r.get(7)?,
                        quantity: r.get(2)?,
                        unit_price: r.get(3)?,
                        amount: r.get(4)?,
                        allocations: Vec::new(),
                        discount: InvoiceDiscount::None,
                        discount_amount: 0,
                        billed_quantity: 0,
                        rate_basis: None,
                    },
                ))
            })?;
            let taxes =
                crate::taxes::tax_details(&self.conn, id, false, primary_tax_name.as_deref(), tax)?;
            let tax = crate::taxes::checked_total(taxes.iter().map(|line| line.amount))?;
            let mut total = i128::from(tax);
            let mut lines = Vec::new();
            for line in line_amounts {
                let (line_id, line) = line?;
                let line = attach_allocation_details(&self.conn, line_id, false, line)?;
                total += i128::from(line.amount);
                lines.push(line);
            }
            let opening_bill = crate::opening_bills::for_invoice(&self.conn, company_id, id)?;
            if let Some(bill) = &opening_bill {
                total += i128::from(bill.amount);
            }
            let adjustments = read_invoice_adjustments(&self.conn, id, false)?;
            for adjustment in &adjustments {
                total += i128::from(adjustment.amount);
            }
            let mut settlement_stmt = self.conn.prepare(
                "SELECT s.amount FROM invoice_settlements s WHERE s.invoice_id = ?1 AND s.company_id = ?2 AND NOT EXISTS(SELECT 1 FROM settlement_reversals r WHERE r.company_id = s.company_id AND r.settlement_voucher_id = s.voucher_id)",
            )?;
            let settlement_rows =
                settlement_stmt.query_map(params![id, company_id], |r| r.get::<_, i64>(0))?;
            let mut settled = 0_i128;
            for amount in settlement_rows {
                settled += i128::from(amount?);
            }
            let has_settlement_history: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM invoice_settlements WHERE invoice_id = ?1 AND company_id = ?2)", params![id, company_id], |r| r.get(0))?;
            let reversal = crate::invoice_reversals::for_invoice(&self.conn, company_id, id)?;
            let advances =
                crate::advances::invoice_allocation_state(&self.conn, company_id, id, None)?;
            let credits = crate::invoice_credits::state(&self.conn, company_id, id, None)?;
            let outstanding = if reversal.is_some() {
                0
            } else {
                total - settled - advances.applied - credits.applied
            };
            let order_ids = crate::orders::linked_orders(&self.conn, id)?;
            let stock_note_ids = crate::stock_notes::linked_notes(&self.conn, id)?;
            result.push(InvoiceSummary {
                id,
                voucher_id,
                kind: InvoiceKind::parse(&kind)?,
                entry_mode: InvoiceEntryMode::parse(&entry_mode)?,
                date,
                due_date,
                credit_days,
                reference,
                external_reference,
                bill_reference,
                bill_splits: crate::bill_splits::read(&self.conn, id, None)?,
                party,
                party_ledger_id,
                total,
                tax_amount: tax,
                taxes,
                tax_calculation: crate::tax_profiles::calculation_detail(&self.conn, id, false)?,
                settled,
                outstanding,
                reversal,
                has_settlement_history,
                has_advance_history: advances.has_history,
                advance_applied: advances.applied,
                credited: credits.applied,
                credit_total: credits.total,
                has_credit_history: credits.has_history,
                has_active_stock_returns: credits.has_active_stock_returns,
                order_id: order_ids.first().copied(),
                stock_note_id: stock_note_ids.first().copied(),
                order_ids,
                stock_note_ids,
                opening_bill,
                lines,
                adjustments,
            });
        }
        Ok(result)
    }
    /// Create and allocate a receipt or payment atomically to one invoice.
    pub fn settle_invoice(
        &mut self,
        company_id: i64,
        invoice_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        amount: i64,
    ) -> Result<i64> {
        let (_, ids) = self.settle_invoices(
            company_id,
            bank_ledger_id,
            date,
            reference,
            &[SettlementAllocation { invoice_id, amount }],
        )?;
        ids.into_iter()
            .next()
            .ok_or_else(|| Error::Validation("No settlement was created".into()))
    }
    /// One receipt or payment may settle several invoices of the same party and type.
    /// All allocations, postings and the voucher are committed together.
    pub fn settle_invoices(
        &mut self,
        company_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        allocations: &[SettlementAllocation],
    ) -> Result<(i64, Vec<i64>)> {
        let allocations = allocations
            .iter()
            .map(|allocation| BillSettlementAllocation {
                target: BillSettlementTarget {
                    invoice_id: allocation.invoice_id,
                    split_id: None,
                },
                amount: allocation.amount,
            })
            .collect::<Vec<_>>();
        self.settle_bills(company_id, bank_ledger_id, date, reference, &allocations)
    }

    /// Post a receipt or payment with explicit allocations to any split bills.
    pub fn settle_bills(
        &mut self,
        company_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        allocations: &[BillSettlementAllocation],
    ) -> Result<(i64, Vec<i64>)> {
        self.settle_bills_with_advance(company_id, bank_ledger_id, date, reference, allocations, 0)
    }

    /// Save one receipt or payment, with an optional amount kept on account.
    pub fn settle_bills_with_advance(
        &mut self,
        company_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        allocations: &[BillSettlementAllocation],
        advance: i64,
    ) -> Result<(i64, Vec<i64>)> {
        if advance < 0 {
            return Err(Error::Validation(
                "Amount kept on account cannot be negative".into(),
            ));
        }
        let single = [(advance, None)];
        let parts = if advance > 0 { &single[..] } else { &[] };
        self.settle_bills_with_parts(
            company_id,
            bank_ledger_id,
            date,
            reference,
            allocations,
            parts,
            &[],
        )
    }

    /// Save bill settlements and several advance amounts on one cash voucher.
    pub fn settle_bills_with_advance_parts(
        &mut self,
        company_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        allocations: &[BillSettlementAllocation],
        advances: &[crate::advances::AdvancePartDraft],
    ) -> Result<(i64, Vec<i64>)> {
        self.settle_bills_with_discounts(
            company_id,
            bank_ledger_id,
            date,
            reference,
            allocations,
            advances,
            &[],
        )
    }

    /// Settle bills for their allocated amount while posting the net cash and discounts separately.
    pub fn settle_bills_with_discounts(
        &mut self,
        company_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        allocations: &[BillSettlementAllocation],
        advances: &[crate::advances::AdvancePartDraft],
        discounts: &[crate::settlement_discounts::SettlementDiscount],
    ) -> Result<(i64, Vec<i64>)> {
        let sources: Vec<_> = advances
            .iter()
            .map(crate::advances::AdvancePartDraft::source_bill)
            .collect();
        let parts: Vec<_> = advances
            .iter()
            .zip(&sources)
            .map(|(advance, source)| (advance.amount, Some(source)))
            .collect();
        self.settle_bills_with_parts(
            company_id,
            bank_ledger_id,
            date,
            reference,
            allocations,
            &parts,
            discounts,
        )
    }

    fn settle_bills_with_parts(
        &mut self,
        company_id: i64,
        bank_ledger_id: i64,
        date: &str,
        reference: &str,
        allocations: &[BillSettlementAllocation],
        parts: &[(i64, Option<&crate::advances::AdvanceSourceBill>)],
        discounts: &[crate::settlement_discounts::SettlementDiscount],
    ) -> Result<(i64, Vec<i64>)> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let advance_voucher = if !parts.is_empty() {
            let advance = i64::try_from(
                parts
                    .iter()
                    .map(|(amount, _)| i128::from(*amount))
                    .sum::<i128>(),
            )
            .map_err(|_| Error::Validation("Advance total is too large".into()))?;
            let first = allocations
                .first()
                .ok_or_else(|| Error::Validation("Select at least one invoice".into()))?;
            let (kind, party): (String, i64) = tx
                .query_row(
                    "SELECT kind, party_ledger_id FROM invoices WHERE company_id = ?1 AND id = ?2",
                    params![company_id, first.target.invoice_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| {
                    Error::Validation("Invoice not found in the selected company".into())
                })?;
            let settled = allocations
                .iter()
                .map(|allocation| i128::from(allocation.amount))
                .sum::<i128>();
            let settled = i64::try_from(settled)
                .map_err(|_| Error::Validation("Settlement amount is too large".into()))?;
            let (_, voucher) = crate::advances::insert_party_advance_parts(
                &tx,
                company_id,
                &crate::advances::AdvanceDraft {
                    kind: InvoiceKind::parse(&kind)?,
                    party_ledger_id: party,
                    bank_ledger_id,
                    date: date.into(),
                    reference: reference.into(),
                    amount: advance,
                    narration: "Invoice settlement and advance".into(),
                },
                parts,
                settled,
                discounts,
                0,
            )?;
            Some(voucher)
        } else {
            None
        };
        let result = insert_settlement_on_connection(
            &tx,
            company_id,
            bank_ledger_id,
            date,
            reference,
            allocations,
            "Invoice settlement",
            advance_voucher,
            discounts,
            0,
        )?;
        tx.commit()?;
        Ok(result)
    }
    /// Reverse a whole receipt or payment, including every invoice allocation it contains.
    pub fn reverse_settlement(
        &mut self,
        company_id: i64,
        settlement_voucher: i64,
        date: &str,
        reference: &str,
        reason: &str,
    ) -> Result<i64> {
        let date = parse_date(date)?;
        let reference = required(reference, "Reversal reference")?;
        let reason = required(reason, "Reversal reason")?;
        let tx = self.conn.transaction()?;
        let voucher = reverse_settlement_on_connection(
            &tx,
            company_id,
            settlement_voucher,
            date,
            reference,
            reason,
        )?;
        tx.commit()?;
        Ok(voucher)
    }
    pub fn invoice_settlements(&self, company_id: i64, invoice_id: i64) -> Result<Vec<Settlement>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.voucher_id, v.date, v.reference, s.amount, l.name,
                    r.reversal_voucher_id, rv.date, r.reason,
                    (SELECT ABS(p.amount) FROM postings p WHERE p.voucher_id = s.voucher_id AND p.ledger_id = s.bank_ledger_id),
                    COALESCE((SELECT SUM(d.amount) FROM settlement_discounts d WHERE d.voucher_id = s.voucher_id), 0)
             FROM invoice_settlements s JOIN vouchers v ON v.id = s.voucher_id
             JOIN ledgers l ON l.id = s.bank_ledger_id AND l.company_id = s.company_id
             LEFT JOIN settlement_reversals r ON r.company_id = s.company_id AND r.settlement_voucher_id = s.voucher_id
             LEFT JOIN vouchers rv ON rv.id = r.reversal_voucher_id
             WHERE s.company_id = ?1 AND s.invoice_id = ?2 ORDER BY v.date, s.id",
        )?;
        let mut rows = stmt
            .query_map(params![company_id, invoice_id], |r| {
                Ok(Settlement {
                    id: r.get(0)?,
                    voucher_id: r.get(1)?,
                    date: r.get(2)?,
                    reference: r.get(3)?,
                    amount: r.get(4)?,
                    bank_ledger: r.get(5)?,
                    reversal_voucher_id: r.get(6)?,
                    reversal_date: r.get(7)?,
                    reversal_reason: r.get(8)?,
                    voucher_cash: r.get(9)?,
                    voucher_discount: r.get(10)?,
                    bill_allocations: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for settlement in &mut rows {
            settlement.bill_allocations =
                crate::bill_splits::settlement_allocations(&self.conn, settlement.id)?;
        }
        Ok(rows)
    }
    /// Unsettled invoices at the end of the requested date. Payments posted
    /// after that date do not affect the historical outstanding amount.
    pub fn outstanding_invoices(
        &self,
        company_id: i64,
        as_of: &str,
    ) -> Result<Vec<OutstandingInvoice>> {
        let end = parse_date(as_of)?;
        let end_text = end.to_string();
        let mut result = Vec::new();
        for invoice in self.invoices(company_id)? {
            let date = parse_date(&invoice.date)?;
            if date > end {
                continue;
            }
            if let Some(reversal) = &invoice.reversal {
                if parse_date(&reversal.date)? <= end {
                    continue;
                }
            }
            let mut settled = 0_i128;
            for settlement in self.invoice_settlements(company_id, invoice.id)? {
                let reversal_date = settlement
                    .reversal_date
                    .as_deref()
                    .map(parse_date)
                    .transpose()?;
                if parse_date(&settlement.date)? <= end
                    && reversal_date.is_none_or(|date| date > end)
                {
                    settled += i128::from(settlement.amount);
                }
            }
            let advance_applied = crate::advances::invoice_allocation_state(
                &self.conn,
                company_id,
                invoice.id,
                Some(end),
            )?
            .applied;
            let credited =
                crate::invoice_credits::state(&self.conn, company_id, invoice.id, Some(end))?
                    .applied;
            let (bill_date, opening_date) = match invoice.opening_bill {
                Some(bill) => (bill.original_date, Some(invoice.date)),
                None => (invoice.date, None),
            };
            let age = (end - parse_date(&bill_date)?).num_days();
            let bills = if invoice.bill_splits.is_empty() {
                vec![(
                    None,
                    invoice.bill_reference,
                    invoice.due_date,
                    invoice.total,
                    settled,
                    advance_applied,
                    credited,
                )]
            } else {
                let bills = crate::bill_splits::read(&self.conn, invoice.id, Some(&end_text))?;
                if bills.iter().map(|bill| bill.settled).sum::<i128>() != settled
                    || bills
                        .iter()
                        .map(|bill| i128::from(bill.amount))
                        .sum::<i128>()
                        != invoice.total
                    || bills.iter().map(|bill| bill.advance_applied).sum::<i128>()
                        != advance_applied
                    || bills.iter().map(|bill| bill.credited).sum::<i128>() != credited
                {
                    return Err(Error::Validation(
                        "Split bill allocations do not reconcile to the invoice".into(),
                    ));
                }
                bills
                    .into_iter()
                    .map(|bill| {
                        (
                            Some(bill.id),
                            bill.reference,
                            bill.due_date,
                            i128::from(bill.amount),
                            bill.settled,
                            bill.advance_applied,
                            bill.credited,
                        )
                    })
                    .collect()
            };
            for (split_id, bill_reference, due_date, total, settled, advance_applied, credited) in
                bills
            {
                let outstanding = total - settled - advance_applied - credited;
                if outstanding < 0 {
                    return Err(Error::Validation(
                        "Allocations and notes exceed the bill total".into(),
                    ));
                }
                if outstanding == 0 {
                    continue;
                }
                let overdue_days = due_date
                    .as_deref()
                    .map(parse_date)
                    .transpose()?
                    .map(|due| (end - due).num_days().max(0));
                result.push(OutstandingInvoice {
                    invoice_id: invoice.id,
                    split_id,
                    opening_date: opening_date.clone(),
                    kind: invoice.kind,
                    entry_mode: invoice.entry_mode,
                    date: bill_date.clone(),
                    due_date,
                    reference: invoice.reference.clone(),
                    bill_reference,
                    party: invoice.party.clone(),
                    party_ledger_id: invoice.party_ledger_id,
                    total,
                    settled,
                    advance_applied,
                    credited,
                    outstanding,
                    age_days: age,
                    overdue_days,
                });
            }
        }
        result.sort_by(|a, b| {
            a.party
                .to_lowercase()
                .cmp(&b.party.to_lowercase())
                .then_with(|| a.date.cmp(&b.date))
                .then_with(|| a.invoice_id.cmp(&b.invoice_id))
        });
        Ok(result)
    }

    /// Group as-of open invoices by party without netting receivables and payables.
    pub fn party_outstanding(&self, company: i64, as_of: &str) -> Result<Vec<PartyOutstanding>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut parties = BTreeMap::<i64, PartyOutstanding>::new();
        let mut seen_invoices = HashSet::new();
        for invoice in self.outstanding_invoices(company, as_of)? {
            let entry =
                parties
                    .entry(invoice.party_ledger_id)
                    .or_insert_with(|| PartyOutstanding {
                        party_ledger_id: invoice.party_ledger_id,
                        party: invoice.party.clone(),
                        ..PartyOutstanding::default()
                    });
            if seen_invoices.insert(invoice.invoice_id) {
                entry.invoices += 1;
            }
            let overdue = invoice.overdue_days.is_some_and(|days| days > 0);
            match invoice.kind {
                InvoiceKind::Sale => {
                    entry.receivable += invoice.outstanding;
                    if overdue {
                        entry.overdue_receivable += invoice.outstanding;
                    }
                }
                InvoiceKind::Purchase => {
                    entry.payable += invoice.outstanding;
                    if overdue {
                        entry.overdue_payable += invoice.outstanding;
                    }
                }
            }
        }
        for (id, party, amount) in
            crate::opening::opening_on_account_as_of(&tx, company, parse_date(as_of)?)?
        {
            parties
                .entry(id)
                .or_insert(PartyOutstanding {
                    party_ledger_id: id,
                    party,
                    ..PartyOutstanding::default()
                })
                .opening_on_account += i128::from(amount);
        }
        let mut result: Vec<_> = parties.into_values().collect();
        result.sort_by(|a, b| {
            a.party
                .to_lowercase()
                .cmp(&b.party.to_lowercase())
                .then_with(|| a.party_ledger_id.cmp(&b.party_ledger_id))
        });
        tx.commit()?;
        Ok(result)
    }
}
