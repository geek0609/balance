#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use balance::banking::{parse_signed_amount, BankEntry, StatementPlan};
use balance::bill_splits::BillSplitDraft;
use balance::company::CompanyProfile;
use balance::contacts::LedgerProfile;
use balance::exchange::{ImportPlan, MAX_XML_IMPORT_BYTES};
use balance::inventory::{
    discounted_line_amount_with_basis, format_quantity, parse_quantity, BillSettlementAllocation,
    BillSettlementTarget, InvoiceAdjustment, InvoiceAllocation, InvoiceDiscount, InvoiceDraft,
    InvoiceEntryMode, InvoiceExternalReference, InvoiceKind, InvoiceLine, InvoiceRateBasis,
    StockBatch, StockItem, StockLocation, StockUnit,
};
use balance::pricing::PriceList;
use balance::reorder::ReorderRule;
use balance::settlement_discounts::SettlementDiscount;
use balance::tax_profiles::TaxCalculation;
use balance::taxes::{format_source_tax_rates, InvoiceTaxDetail, InvoiceTaxLine};
use balance::terms::LedgerTerms;
use balance::{
    format_amount, parse_amount, Category, Ledger, Posting, Store, VoucherDraft, VoucherKind,
};
use eframe::egui;

mod actual_cost_ui;
mod advance_ui;
mod bill_note_credit_ui;
mod budget_ui;
mod cash_flow_ui;
mod cost_ui;
mod currency_ui;
mod invoice_credit_ui;
mod invoice_refund_ui;
mod manufacturing_ui;
mod opening_bill_ui;
mod order_ui;
mod payroll_ui;
mod recurring_ui;
mod shell;
mod stock_count_ui;
mod stock_note_ui;
mod stock_policy_ui;
mod stock_transit_ui;
mod tax_ui;
mod theme;
#[cfg(all(windows, target_arch = "x86_64", feature = "windows-updater"))]
mod updates;
mod valuation_ui;
use actual_cost_ui::ActualCostScreen;
use advance_ui::AdvanceScreen;
use budget_ui::BudgetScreen;
use cash_flow_ui::CashFlowScreen;
use cost_ui::CostScreen;
use currency_ui::CurrencyScreen;
use manufacturing_ui::ManufacturingScreen;
use order_ui::OrderScreen;
use payroll_ui::PayrollScreen;
use recurring_ui::RecurringJournalScreen;
use stock_count_ui::StockCountScreen;
use stock_note_ui::StockNoteScreen;
use tax_ui::{TaxProfileScreen, TaxRegisterScreen};
use valuation_ui::ValuationScreen;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Page {
    Home,
    Company,
    Ledger,
    Voucher,
    Register,
    Import,
    Stock,
    StockCounts,
    StockNotes,
    Valuation,
    Manufacturing,
    Invoice,
    InvoiceRegister,
    Orders,
    Backup,
    DayBook,
    Outstanding,
    Advances,
    Bank,
    Reports,
    CostCentres,
    Budgets,
    TaxRegister,
    Payroll,
    Currencies,
    CashFlow,
    Recurring,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum InvoiceSource {
    Order(i64),
    StockNotes,
}

struct App {
    store: Store,
    shell: shell::Shell,
    opening_bill: opening_bill_ui::OpeningBillForm,
    #[cfg(all(windows, target_arch = "x86_64", feature = "windows-updater"))]
    updates: updates::Updates,
    database_path: std::path::PathBuf,
    page: Page,
    company: Option<i64>,
    company_name: String,
    company_profile_selection: Option<i64>,
    company_profile: CompanyProfile,
    company_profile_original: CompanyProfile,
    period_lock_date: String,
    period_lock_reason: String,
    ledger_name: String,
    category: Category,
    ledger_group: Option<i64>,
    editing_ledger: Option<i64>,
    ledger_edit_name: String,
    ledger_edit_group: Option<i64>,
    group_name: String,
    group_parent: Option<i64>,
    profile_ledger: Option<i64>,
    profile_selection: Option<i64>,
    ledger_profile: LedgerProfile,
    profile_original: LedgerProfile,
    terms_selection: Option<i64>,
    terms_ledger: Option<i64>,
    terms_days: String,
    terms_limit: String,
    terms_original_days: String,
    terms_original_limit: String,
    opening_ledger: Option<i64>,
    opening_date: String,
    opening_amount: String,
    opening_debit: bool,
    opening_on_account: bool,
    opening_clear_date: String,
    opening_clear_reference: String,
    opening_clear_reason: String,
    lines: Vec<VoucherLine>,
    editing_voucher: Option<i64>,
    voucher_reversal_target: Option<i64>,
    voucher_reversal_date: String,
    voucher_reversal_reference: String,
    voucher_reversal_reason: String,
    voucher_kind: VoucherKind,
    voucher_reference: String,
    date: String,
    narration: String,
    report_from: String,
    report_to: String,
    register_ledger: Option<i64>,
    ledger_statement_path: String,
    register_from: String,
    register_to: String,
    day_from: String,
    day_to: String,
    day_kind: Option<VoucherKind>,
    day_search: String,
    report_export_path: String,
    outstanding_date: String,
    invoice_register_from: String,
    invoice_register_to: String,
    invoice_register_kind: Option<InvoiceKind>,
    invoice_register_party: Option<i64>,
    invoice_register_search: String,
    bank_ledger: Option<i64>,
    bank_as_of: String,
    bank_posting: Option<i64>,
    bank_clearance_date: String,
    bank_reason: String,
    bank_csv_path: String,
    bank_csv_preview: Option<StatementPlan>,
    bank_statement_selection: Option<i64>,
    bank_match_reason: String,
    bank_opening_date: String,
    bank_opening_amount: String,
    bank_opening_reason: String,
    import_path: String,
    import_second_path: String,
    import_opening_date: String,
    backup_path: String,
    restore_source: String,
    restore_destination: String,
    import_preview: Option<ImportPlan>,
    stock_name: String,
    stock_description: String,
    stock_creation_unit: Option<i64>,
    stock_creation_group: Option<i64>,
    stock_edit_selection: Option<i64>,
    stock_editing: Option<i64>,
    stock_edit_name: String,
    stock_edit_group: Option<i64>,
    stock_edit_description: String,
    unit_symbol: String,
    unit_formal_name: String,
    unit_decimals: u8,
    compound_unit: CompoundUnitForm,
    stock_group_name: String,
    stock_group_parent: Option<i64>,
    stock_location_name: String,
    stock_location_parent: Option<i64>,
    batch_item: Option<i64>,
    batch_name: String,
    batch_manufactured: String,
    batch_expiry: String,
    stock_balance_location: Option<i64>,
    stock_balance_descendants: bool,
    adjustment_location: Option<i64>,
    adjustment_batch: Option<i64>,
    transfer_item: Option<i64>,
    transfer_from: Option<i64>,
    transfer_to: Option<i64>,
    transfer_batch: Option<i64>,
    transfer_date: String,
    transfer_quantity: String,
    transfer_reason: String,
    stock_transit_screen: stock_transit_ui::StockTransitScreen,
    negative_stock_screen: stock_policy_ui::NegativeStockScreen,
    stock_as_of: String,
    reorder_selection: Option<i64>,
    reorder_editing: Option<i64>,
    reorder_enabled: bool,
    reorder_level: String,
    reorder_quantity: String,
    price_list_name: String,
    price_list: Option<i64>,
    price_item: Option<i64>,
    price_date: String,
    price_amount: String,
    price_reason: String,
    stock_register_item: Option<i64>,
    stock_register_from: String,
    stock_register_to: String,
    adjustment_item: Option<i64>,
    adjustment_date: String,
    adjustment_quantity: String,
    adjustment_increase: bool,
    adjustment_reason: String,
    invoice_kind: InvoiceKind,
    invoice_entry_mode: InvoiceEntryMode,
    invoice_date: String,
    invoice_due_date: String,
    invoice_credit_days: String,
    invoice_reference: String,
    invoice_external_number: String,
    invoice_external_date: String,
    invoice_bill_reference: String,
    invoice_bill_splits: Vec<BillSplitForm>,
    invoice_narration: String,
    invoice_party: Option<i64>,
    invoice_trade: Option<i64>,
    invoice_tax_ledger: Option<i64>,
    invoice_tax_amount: String,
    invoice_additional_taxes: Vec<InvoiceTaxForm>,
    invoice_tax_calculation: Option<TaxCalculation>,
    invoice_tax_calculation_date: String,
    invoice_tax_profile: Option<i64>,
    invoice_taxable_base: String,
    invoice_lines: Vec<InvoiceLineForm>,
    invoice_adjustments: Vec<InvoiceAdjustmentForm>,
    editing_invoice: Option<i64>,
    copying_invoice: bool,
    invoice_source: Option<InvoiceSource>,
    order_screen: OrderScreen,
    stock_count_screen: StockCountScreen,
    stock_note_screen: StockNoteScreen,
    cost_screen: CostScreen,
    valuation_screen: ValuationScreen,
    actual_cost_screen: ActualCostScreen,
    advance_screen: AdvanceScreen,
    manufacturing_screen: ManufacturingScreen,
    budget_screen: BudgetScreen,
    recurring_journals: RecurringJournalScreen,
    tax_register_screen: TaxRegisterScreen,
    tax_profile_screen: TaxProfileScreen,
    payroll_screen: PayrollScreen,
    currency_screen: CurrencyScreen,
    cash_flow_screen: CashFlowScreen,
    invoice_document_path: String,
    settlement_invoice: Option<BillSettlementTarget>,
    settlement_bank: Option<i64>,
    settlement_date: String,
    settlement_reference: String,
    settlement_amount: String,
    settlement_on_account: String,
    settlement_advances: Vec<advance_ui::AdvancePartForm>,
    settlement_discounts: Vec<SettlementDiscountForm>,
    settlement_extra: Vec<SettlementLineForm>,
    reversal_target: Option<i64>,
    reversal_date: String,
    reversal_reference: String,
    reversal_reason: String,
    invoice_reversal: Option<InvoiceReversalForm>,
    invoice_credit: Option<invoice_credit_ui::NoteForm>,
    invoice_refund: Option<invoice_refund_ui::RefundForm>,
    note_credit_action: Option<bill_note_credit_ui::NoteCreditAction>,
    message: String,
}
struct InvoiceReversalForm {
    invoice_id: i64,
    date: String,
    reference: String,
    reason: String,
}
struct SettlementLineForm {
    target: Option<BillSettlementTarget>,
    amount: String,
}
#[derive(Default)]
struct SettlementDiscountForm {
    ledger: Option<i64>,
    amount: String,
}
#[derive(Default)]
struct BillSplitForm {
    reference: String,
    amount: String,
    due_date: String,
    credit_days: String,
}
impl BillSplitForm {
    fn draft(&self) -> balance::Result<BillSplitDraft> {
        let credit_days = self.credit_days.trim();
        Ok(BillSplitDraft {
            reference: self.reference.clone(),
            amount: parse_amount(&self.amount)?,
            due_date: (!self.due_date.trim().is_empty()).then(|| self.due_date.trim().to_owned()),
            credit_days: if credit_days.is_empty() {
                None
            } else {
                Some(credit_days.parse().map_err(|_| {
                    balance::Error::Validation("Use whole credit days for each split bill".into())
                })?)
            },
        })
    }
}
#[derive(Default)]
struct InvoiceTaxForm {
    ledger: Option<i64>,
    amount: String,
    original: Option<InvoiceTaxLine>,
}
impl InvoiceTaxForm {
    fn source_rates(&self, amount: i64) -> &[rust_decimal::Decimal] {
        self.original
            .as_ref()
            .filter(|original| self.ledger == Some(original.ledger_id) && amount == original.amount)
            .map_or(&[], |original| original.source_rates.as_slice())
    }
}
struct InvoiceAdjustmentForm {
    ledger: Option<i64>,
    amount: String,
    increase: bool,
    description: String,
}
struct InvoiceLineForm {
    item: Option<i64>,
    stock_note_line: Option<(i64, String)>,
    location: Option<i64>,
    batch: Option<i64>,
    trade_ledger: Option<i64>,
    quantity: String,
    billed_quantity: String,
    price: String,
    rate_basis: Option<InvoiceRateBasis>,
    discount_mode: DiscountMode,
    discount_value: String,
    extra_allocations: Vec<InvoiceAllocationForm>,
}

#[derive(Default)]
struct CompoundUnitForm {
    primary: Option<i64>,
    tail: Option<i64>,
    conversion: String,
    symbol: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiscountMode {
    None,
    Fixed,
    Percent,
}
struct InvoiceAllocationForm {
    location: Option<i64>,
    batch: Option<i64>,
    quantity: String,
    billed_quantity: String,
}
impl InvoiceLineForm {
    fn new() -> Self {
        Self {
            item: None,
            stock_note_line: None,
            location: None,
            batch: None,
            trade_ledger: None,
            quantity: String::new(),
            billed_quantity: String::new(),
            price: String::new(),
            rate_basis: None,
            discount_mode: DiscountMode::None,
            discount_value: String::new(),
            extra_allocations: Vec::new(),
        }
    }
    fn price_amount(&self) -> balance::Result<i64> {
        if self.price.trim().is_empty()
            && parse_optional_quantity(&self.billed_quantity)? == Some(0)
        {
            Ok(0)
        } else {
            parse_amount(&self.price)
        }
    }

    fn discount(&self) -> balance::Result<InvoiceDiscount> {
        match self.discount_mode {
            DiscountMode::None => Ok(InvoiceDiscount::None),
            DiscountMode::Fixed => Ok(InvoiceDiscount::Fixed(parse_amount(&self.discount_value)?)),
            DiscountMode::Percent => Ok(InvoiceDiscount::Percent(
                self.discount_value.trim().to_owned(),
            )),
        }
    }
}
fn parse_optional_quantity(value: &str) -> balance::Result<Option<i64>> {
    (!value.trim().is_empty())
        .then(|| parse_quantity(value))
        .transpose()
}

struct VoucherLine {
    ledger: Option<i64>,
    debit: bool,
    amount: String,
    is_party: bool,
}
impl VoucherLine {
    fn new(debit: bool) -> Self {
        Self {
            ledger: None,
            debit,
            amount: String::new(),
            is_party: false,
        }
    }
}
impl App {
    fn new(store: Store, database_path: std::path::PathBuf) -> Self {
        Self {
            store,
            shell: shell::Shell::default(),
            opening_bill: opening_bill_ui::OpeningBillForm::default(),
            #[cfg(all(windows, target_arch = "x86_64", feature = "windows-updater"))]
            updates: updates::Updates::default(),
            database_path,
            page: Page::Home,
            company: None,
            company_name: String::new(),
            company_profile_selection: None,
            company_profile: CompanyProfile::default(),
            company_profile_original: CompanyProfile::default(),
            period_lock_date: String::new(),
            period_lock_reason: String::new(),
            ledger_name: String::new(),
            category: Category::Asset,
            ledger_group: None,
            editing_ledger: None,
            ledger_edit_name: String::new(),
            ledger_edit_group: None,
            group_name: String::new(),
            group_parent: None,
            profile_ledger: None,
            profile_selection: None,
            ledger_profile: LedgerProfile::default(),
            profile_original: LedgerProfile::default(),
            terms_selection: None,
            terms_ledger: None,
            terms_days: String::new(),
            terms_limit: String::new(),
            terms_original_days: String::new(),
            terms_original_limit: String::new(),
            opening_ledger: None,
            opening_date: String::new(),
            opening_amount: String::new(),
            opening_debit: true,
            opening_on_account: false,
            opening_clear_date: String::new(),
            opening_clear_reference: String::new(),
            opening_clear_reason: String::new(),
            lines: vec![VoucherLine::new(true), VoucherLine::new(false)],
            editing_voucher: None,
            voucher_reversal_target: None,
            voucher_reversal_date: String::new(),
            voucher_reversal_reference: String::new(),
            voucher_reversal_reason: String::new(),
            voucher_kind: VoucherKind::Journal,
            voucher_reference: String::new(),
            date: String::new(),
            narration: String::new(),
            report_from: String::new(),
            report_to: String::new(),
            register_ledger: None,
            ledger_statement_path: String::new(),
            register_from: String::new(),
            register_to: String::new(),
            day_from: String::new(),
            day_to: String::new(),
            day_kind: None,
            day_search: String::new(),
            report_export_path: String::new(),
            outstanding_date: String::new(),
            invoice_register_from: String::new(),
            invoice_register_to: String::new(),
            invoice_register_kind: None,
            invoice_register_party: None,
            invoice_register_search: String::new(),
            bank_ledger: None,
            bank_as_of: String::new(),
            bank_posting: None,
            bank_clearance_date: String::new(),
            bank_reason: String::new(),
            bank_csv_path: String::new(),
            bank_csv_preview: None,
            bank_statement_selection: None,
            bank_match_reason: String::new(),
            bank_opening_date: String::new(),
            bank_opening_amount: String::new(),
            bank_opening_reason: String::new(),
            import_path: String::new(),
            import_second_path: String::new(),
            import_opening_date: String::new(),
            backup_path: String::new(),
            restore_source: String::new(),
            restore_destination: String::new(),
            import_preview: None,
            stock_name: String::new(),
            stock_description: String::new(),
            stock_creation_unit: None,
            stock_creation_group: None,
            stock_edit_selection: None,
            stock_editing: None,
            stock_edit_name: String::new(),
            stock_edit_group: None,
            stock_edit_description: String::new(),
            unit_symbol: String::new(),
            unit_formal_name: String::new(),
            unit_decimals: 0,
            compound_unit: CompoundUnitForm::default(),
            stock_group_name: String::new(),
            stock_group_parent: None,
            stock_location_name: String::new(),
            stock_location_parent: None,
            batch_item: None,
            batch_name: String::new(),
            batch_manufactured: String::new(),
            batch_expiry: String::new(),
            stock_balance_location: None,
            stock_balance_descendants: false,
            adjustment_location: None,
            adjustment_batch: None,
            transfer_item: None,
            transfer_from: None,
            transfer_to: None,
            transfer_batch: None,
            transfer_date: String::new(),
            transfer_quantity: String::new(),
            transfer_reason: String::new(),
            stock_transit_screen: stock_transit_ui::StockTransitScreen::default(),
            negative_stock_screen: stock_policy_ui::NegativeStockScreen::default(),
            stock_as_of: String::new(),
            reorder_selection: None,
            reorder_editing: None,
            reorder_enabled: true,
            reorder_level: String::new(),
            reorder_quantity: String::new(),
            price_list_name: String::new(),
            price_list: None,
            price_item: None,
            price_date: String::new(),
            price_amount: String::new(),
            price_reason: String::new(),
            stock_register_item: None,
            stock_register_from: String::new(),
            stock_register_to: String::new(),
            adjustment_item: None,
            adjustment_date: String::new(),
            adjustment_quantity: String::new(),
            adjustment_increase: true,
            adjustment_reason: String::new(),
            invoice_kind: InvoiceKind::Sale,
            invoice_entry_mode: InvoiceEntryMode::Invoice,
            invoice_date: String::new(),
            invoice_due_date: String::new(),
            invoice_credit_days: String::new(),
            invoice_reference: String::new(),
            invoice_external_number: String::new(),
            invoice_external_date: String::new(),
            invoice_bill_reference: String::new(),
            invoice_bill_splits: Vec::new(),
            invoice_narration: String::new(),
            invoice_party: None,
            invoice_trade: None,
            invoice_tax_ledger: None,
            invoice_tax_amount: String::new(),
            invoice_additional_taxes: Vec::new(),
            invoice_tax_calculation: None,
            invoice_tax_calculation_date: String::new(),
            invoice_tax_profile: None,
            invoice_taxable_base: String::new(),
            invoice_lines: vec![InvoiceLineForm::new()],
            invoice_adjustments: Vec::new(),
            editing_invoice: None,
            copying_invoice: false,
            invoice_source: None,
            order_screen: OrderScreen::default(),
            stock_count_screen: StockCountScreen::default(),
            stock_note_screen: StockNoteScreen::default(),
            cost_screen: CostScreen::default(),
            valuation_screen: ValuationScreen::default(),
            actual_cost_screen: ActualCostScreen::default(),
            advance_screen: AdvanceScreen::default(),
            manufacturing_screen: ManufacturingScreen::default(),
            budget_screen: BudgetScreen::default(),
            recurring_journals: RecurringJournalScreen::default(),
            tax_register_screen: TaxRegisterScreen::default(),
            tax_profile_screen: TaxProfileScreen::default(),
            payroll_screen: PayrollScreen::default(),
            currency_screen: CurrencyScreen::default(),
            cash_flow_screen: CashFlowScreen::default(),
            invoice_document_path: String::new(),
            settlement_invoice: None,
            settlement_bank: None,
            settlement_date: String::new(),
            settlement_reference: String::new(),
            settlement_amount: String::new(),
            settlement_on_account: String::new(),
            settlement_advances: Vec::new(),
            settlement_discounts: Vec::new(),
            settlement_extra: Vec::new(),
            reversal_target: None,
            reversal_date: String::new(),
            reversal_reference: String::new(),
            reversal_reason: String::new(),
            invoice_reversal: None,
            invoice_credit: None,
            invoice_refund: None,
            note_credit_action: None,
            message: String::new(),
        }
    }
    fn failure(&mut self, e: impl std::fmt::Display) {
        self.message = e.to_string();
        self.shell.last_failure = self.message.clone();
    }
    fn clear_voucher(&mut self) {
        self.lines = vec![VoucherLine::new(true), VoucherLine::new(false)];
        self.editing_voucher = None;
        self.voucher_kind = VoucherKind::Journal;
        self.voucher_reference.clear();
        self.narration.clear();
    }
    fn reset_company_forms(&mut self) {
        self.opening_bill = opening_bill_ui::OpeningBillForm::default();
        self.company_profile_selection = None;
        self.company_profile = CompanyProfile::default();
        self.company_profile_original = CompanyProfile::default();
        self.period_lock_date.clear();
        self.period_lock_reason.clear();
        self.clear_voucher();
        self.voucher_reversal_target = None;
        self.voucher_reversal_date.clear();
        self.voucher_reversal_reference.clear();
        self.voucher_reversal_reason.clear();
        self.register_ledger = None;
        self.ledger_group = None;
        self.editing_ledger = None;
        self.ledger_edit_name.clear();
        self.ledger_edit_group = None;
        self.group_parent = None;
        self.profile_ledger = None;
        self.profile_selection = None;
        self.ledger_profile = LedgerProfile::default();
        self.profile_original = LedgerProfile::default();
        self.terms_selection = None;
        self.terms_ledger = None;
        self.terms_days.clear();
        self.terms_limit.clear();
        self.terms_original_days.clear();
        self.terms_original_limit.clear();
        self.opening_ledger = None;
        self.invoice_register_party = None;
        self.bank_ledger = None;
        self.bank_posting = None;
        self.bank_clearance_date.clear();
        self.bank_reason.clear();
        self.bank_csv_preview = None;
        self.bank_statement_selection = None;
        self.bank_match_reason.clear();
        self.bank_opening_date.clear();
        self.bank_opening_amount.clear();
        self.bank_opening_reason.clear();
        self.opening_date.clear();
        self.opening_amount.clear();
        self.opening_on_account = false;
        self.opening_clear_date.clear();
        self.opening_clear_reference.clear();
        self.opening_clear_reason.clear();
        self.import_preview = None;
        self.adjustment_item = None;
        self.stock_creation_unit = None;
        self.compound_unit = CompoundUnitForm::default();
        self.stock_creation_group = None;
        self.stock_edit_selection = None;
        self.stock_editing = None;
        self.stock_edit_name.clear();
        self.stock_edit_group = None;
        self.stock_edit_description.clear();
        self.stock_group_parent = None;
        self.stock_location_parent = None;
        self.stock_balance_location = None;
        self.stock_balance_descendants = false;
        self.reorder_selection = None;
        self.reorder_editing = None;
        self.reorder_level.clear();
        self.reorder_quantity.clear();
        self.price_list_name.clear();
        self.price_list = None;
        self.price_item = None;
        self.price_date.clear();
        self.price_amount.clear();
        self.price_reason.clear();
        self.batch_item = None;
        self.batch_name.clear();
        self.batch_manufactured.clear();
        self.batch_expiry.clear();
        self.adjustment_location = None;
        self.adjustment_batch = None;
        self.transfer_item = None;
        self.transfer_from = None;
        self.transfer_to = None;
        self.transfer_batch = None;
        self.transfer_date.clear();
        self.transfer_quantity.clear();
        self.transfer_reason.clear();
        self.stock_transit_screen = stock_transit_ui::StockTransitScreen::default();
        self.negative_stock_screen = stock_policy_ui::NegativeStockScreen::default();
        self.stock_register_item = None;
        self.invoice_party = None;
        self.invoice_trade = None;
        self.invoice_tax_ledger = None;
        self.invoice_lines = vec![InvoiceLineForm::new()];
        self.invoice_adjustments.clear();
        self.editing_invoice = None;
        self.copying_invoice = false;
        self.invoice_source = None;
        self.order_screen = OrderScreen::default();
        self.stock_count_screen = StockCountScreen::default();
        self.stock_note_screen = StockNoteScreen::default();
        self.cost_screen = CostScreen::default();
        self.valuation_screen = ValuationScreen::default();
        self.actual_cost_screen = ActualCostScreen::default();
        self.advance_screen = AdvanceScreen::default();
        self.manufacturing_screen = ManufacturingScreen::default();
        self.budget_screen = BudgetScreen::default();
        self.recurring_journals = RecurringJournalScreen::default();
        self.tax_register_screen = TaxRegisterScreen::default();
        self.tax_profile_screen = TaxProfileScreen::default();
        self.payroll_screen = PayrollScreen::default();
        self.currency_screen = CurrencyScreen::default();
        self.cash_flow_screen = CashFlowScreen::default();
        self.invoice_reference.clear();
        self.invoice_external_number.clear();
        self.invoice_external_date.clear();
        self.invoice_bill_reference.clear();
        self.invoice_bill_splits.clear();
        self.invoice_narration.clear();
        self.invoice_due_date.clear();
        self.invoice_credit_days.clear();
        self.invoice_tax_amount.clear();
        self.invoice_additional_taxes.clear();
        self.clear_invoice_tax_context();
        self.settlement_invoice = None;
        self.settlement_bank = None;
        self.settlement_reference.clear();
        self.settlement_amount.clear();
        self.settlement_on_account.clear();
        self.settlement_advances.clear();
        self.settlement_discounts.clear();
        self.settlement_extra.clear();
        self.reversal_target = None;
        self.reversal_date.clear();
        self.reversal_reference.clear();
        self.reversal_reason.clear();
        self.invoice_reversal = None;
        self.invoice_credit = None;
        self.invoice_refund = None;
        self.note_credit_action = None;
    }
    /// Whether a form holds edits that would be lost by switching company.
    fn has_unsaved_edits(&self) -> bool {
        (self.profile_ledger.is_some() && self.ledger_profile != self.profile_original)
            || self.company_profile != self.company_profile_original
            || (self.terms_ledger.is_some()
                && (self.terms_days != self.terms_original_days
                    || self.terms_limit != self.terms_original_limit))
            || self.editing_ledger.is_some()
            || self.invoice_draft_in_use()
            || self.invoice_refund.is_some()
            || self.note_credit_action.is_some()
            || self.order_screen.amending_order()
            || self.order_screen.revising_quotation()
            || self.stock_count_screen.preview_pending()
            || self.cost_screen.has_pending_changes()
            || self.currency_screen.has_preview()
            || self.stock_editing.is_some()
            || self.reorder_editing.is_some()
    }
    fn company_page(&mut self, ui: &mut egui::Ui) {
        crate::theme::section(ui, "Companies");
        ui.label("Create or select a company to work with.");
        if self.company != self.company_profile_selection {
            if let Some(company) = self.company {
                match self.store.company_profile(company) {
                    Ok(profile) => {
                        self.company_profile = profile.clone();
                        self.company_profile_original = profile;
                        self.company_profile_selection = Some(company);
                    }
                    Err(e) => {
                        self.failure(e);
                        return;
                    }
                }
            }
        }
        let profile_dirty = self.has_unsaved_edits();
        if profile_dirty {
            ui.label("Save or discard the current edits before changing company.");
        }
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.company_name);
            if ui
                .add_enabled(!profile_dirty, egui::Button::new("Create company"))
                .clicked()
            {
                match self.store.create_company(&self.company_name) {
                    Ok(id) => {
                        self.company = Some(id);
                        self.reset_company_forms();
                        self.company_name.clear();
                        self.message = "Company created".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        match self.store.companies() {
            Ok(companies) => {
                for company in companies {
                    if ui
                        .add_enabled(
                            !profile_dirty,
                            egui::Button::selectable(
                                self.company == Some(company.id),
                                &company.name,
                            ),
                        )
                        .clicked()
                    {
                        self.company = Some(company.id);
                        self.reset_company_forms();
                        self.message.clear();
                    }
                }
            }
            Err(e) => self.failure(e),
        }
        if let Some(company) = self.company {
            ui.separator();
            crate::theme::section(ui, "Company details");
            ui.small("Financial year and books start dates are for reference.");
            egui::Grid::new("company-profile")
                .num_columns(2)
                .show(ui, |ui| {
                    for (label, value) in [
                        ("Address", &mut self.company_profile.address),
                        ("Region", &mut self.company_profile.region),
                        ("Postal code", &mut self.company_profile.postal_code),
                        ("Country", &mut self.company_profile.country),
                        ("Email", &mut self.company_profile.email),
                        ("Phone", &mut self.company_profile.phone),
                        ("Tax ID", &mut self.company_profile.tax_id),
                        (
                            "Registration type",
                            &mut self.company_profile.registration_type,
                        ),
                        (
                            "Financial year start (YYYY-MM-DD)",
                            &mut self.company_profile.financial_year_start,
                        ),
                        (
                            "Books start (YYYY-MM-DD)",
                            &mut self.company_profile.books_start,
                        ),
                    ] {
                        ui.label(label);
                        ui.text_edit_singleline(value);
                        ui.end_row();
                    }
                });
            ui.horizontal(|ui| {
                if ui.button("Save company details").clicked() {
                    match self
                        .store
                        .save_company_profile(company, &self.company_profile)
                        .and_then(|()| self.store.company_profile(company))
                    {
                        Ok(profile) => {
                            self.company_profile = profile.clone();
                            self.company_profile_original = profile;
                            self.message = "Company details saved".into();
                        }
                        Err(e) => self.failure(e),
                    }
                }
                if ui.button("Discard company detail edits").clicked() {
                    self.company_profile = self.company_profile_original.clone();
                }
            });
            egui::CollapsingHeader::new("Company detail history").show(ui, |ui| {
                match self.store.company_profile_revisions(company) {
                    Ok(rows) if rows.is_empty() => { ui.label("No earlier saved versions."); }
                    Ok(rows) => for row in rows {
                        ui.label(format!("{} UTC — {}, {}, {}, {} | {} | {} | Tax ID: {} | Registration: {} | Financial year: {} | Books: {}", row.changed_at, row.profile.address, row.profile.region, row.profile.postal_code, row.profile.country, row.profile.email, row.profile.phone, row.profile.tax_id, row.profile.registration_type, row.profile.financial_year_start, row.profile.books_start));
                    },
                    Err(e) => { ui.colored_label(ui.visuals().error_fg_color, e.to_string()); }
                }
            });
            self.period_lock_form(ui, company);
        }
    }

    fn period_lock_form(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.separator();
        crate::theme::section(ui, "Period lock");
        let cutoff = match self.store.period_lock(company) {
            Ok(cutoff) => cutoff,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        if let Some(date) = cutoff {
            ui.label(format!("Transactions locked through {date}"));
        } else {
            ui.label("All periods are open.");
        }
        ui.small(
            "The lock applies to vouchers, stock counts and stock movements, including imports.",
        );
        egui::Grid::new("period-lock").show(ui, |ui| {
            ui.label("Lock through (YYYY-MM-DD)");
            ui.text_edit_singleline(&mut self.period_lock_date);
            ui.end_row();
            ui.label("Reason");
            ui.text_edit_singleline(&mut self.period_lock_reason);
            ui.end_row();
        });
        let mut change = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.period_lock_date.trim().is_empty()
                        && !self.period_lock_reason.trim().is_empty(),
                    egui::Button::new("Set cutoff"),
                )
                .clicked()
            {
                change = Some(Some(self.period_lock_date.as_str()));
            }
            if ui
                .add_enabled(
                    cutoff.is_some() && !self.period_lock_reason.trim().is_empty(),
                    egui::Button::new("Reopen all periods"),
                )
                .clicked()
            {
                change = Some(None);
            }
        });
        if let Some(date) = change {
            match self
                .store
                .set_period_lock(company, date, &self.period_lock_reason)
            {
                Ok(()) => {
                    self.period_lock_date.clear();
                    self.period_lock_reason.clear();
                    self.message = "Period lock updated".into();
                }
                Err(error) => self.failure(error),
            }
        }
        egui::CollapsingHeader::new("Period lock history").show(ui, |ui| {
            match self.store.period_lock_history(company) {
                Ok(events) if events.is_empty() => {
                    ui.label("No changes recorded.");
                }
                Ok(events) => {
                    for event in events {
                        let state = match event.locked_through {
                            Some(date) => format!("Locked through {date}"),
                            None => "All periods reopened".into(),
                        };
                        ui.label(format!(
                            "{} UTC | {state} | {}",
                            event.changed_at, event.reason
                        ));
                    }
                }
                Err(error) => self.failure(error),
            }
        });
    }

    fn ledger_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Ledgers");
        let groups = match self.store.account_groups(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.ledger_name);
            let group_name = groups
                .iter()
                .find(|g| Some(g.id) == self.ledger_group)
                .map(|g| g.name.as_str())
                .unwrap_or("Select group");
            egui::ComboBox::from_label("Group")
                .selected_text(group_name)
                .show_ui(ui, |ui| {
                    for group in &groups {
                        ui.selectable_value(&mut self.ledger_group, Some(group.id), &group.name);
                    }
                });
        });
        if ui.button("Create ledger").clicked() {
            let result = match self.ledger_group {
                Some(group) => self
                    .store
                    .create_ledger_in_group(company, &self.ledger_name, group),
                None => Err(balance::Error::Validation("Select an account group".into())),
            };
            match result {
                Ok(_) => {
                    self.ledger_name.clear();
                    self.message = "Ledger created".into();
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        let ledgers = match self.store.ledgers(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        {
            for ledger in &ledgers {
                let group = groups
                    .iter()
                    .find(|g| Some(g.id) == ledger.group_id)
                    .map(|g| g.name.as_str())
                    .unwrap_or(ledger.category.as_str());
                ui.horizontal(|ui| {
                    ui.label(format!("{}  —  {}", ledger.name, group));
                    if ui
                        .add_enabled(
                            self.editing_ledger.is_none(),
                            egui::Button::new("Edit ledger"),
                        )
                        .clicked()
                    {
                        self.editing_ledger = Some(ledger.id);
                        self.ledger_edit_name = ledger.name.clone();
                        self.ledger_edit_group = ledger.group_id;
                    }
                });
                egui::CollapsingHeader::new("Previous ledger names and groups")
                    .id_salt(("ledger-revisions", ledger.id))
                    .show(ui, |ui| {
                        match self.store.ledger_revisions(company, ledger.id) {
                            Ok(rows) if rows.is_empty() => {
                                ui.label("No earlier versions.");
                            }
                            Ok(rows) => {
                                for row in rows {
                                    ui.label(format!(
                                        "{} UTC — {} — {}",
                                        row.changed_at,
                                        row.name,
                                        row.group.as_deref().unwrap_or("unassigned")
                                    ));
                                }
                            }
                            Err(e) => {
                                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                            }
                        }
                    });
            }
        }
        if let Some(id) = self.editing_ledger {
            if let Some(ledger) = ledgers.iter().find(|ledger| ledger.id == id) {
                ui.separator();
                crate::theme::section(ui, format!("Edit ledger: {}", ledger.name));
                ui.small("The group must retain the same accounting category; existing reports use the current ledger name and group.");
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.ledger_edit_name);
                    let group_name = groups
                        .iter()
                        .find(|g| Some(g.id) == self.ledger_edit_group)
                        .map(|g| g.name.as_str())
                        .unwrap_or("Select group");
                    egui::ComboBox::from_id_salt("edit-ledger-group")
                        .selected_text(group_name)
                        .show_ui(ui, |ui| {
                            for group in groups.iter().filter(|g| g.category == ledger.category) {
                                ui.selectable_value(
                                    &mut self.ledger_edit_group,
                                    Some(group.id),
                                    &group.name,
                                );
                            }
                        });
                });
                ui.horizontal(|ui| {
                    if ui.button("Save ledger changes").clicked() {
                        let result = self
                            .ledger_edit_group
                            .ok_or_else(|| balance::Error::Validation("Select a group".into()))
                            .and_then(|group| {
                                self.store
                                    .update_ledger(company, id, &self.ledger_edit_name, group)
                            });
                        match result {
                            Ok(()) => {
                                self.editing_ledger = None;
                                self.message = "Ledger updated".into();
                            }
                            Err(e) => self.failure(e),
                        }
                    }
                    if ui.button("Cancel ledger edit").clicked() {
                        self.editing_ledger = None;
                    }
                });
            }
        }
        ui.separator();
        crate::theme::section(ui, "Opening balances");
        ui.label("Each opening balance is offset against the opening difference account. Reconcile the difference as other opening balances are entered.");
        let previous_opening_ledger = self.opening_ledger;
        pick_account(
            ui,
            "opening-ledger",
            "Ledger",
            &ledgers,
            &mut self.opening_ledger,
            false,
        );
        if previous_opening_ledger != self.opening_ledger {
            self.opening_date.clear();
            self.opening_amount.clear();
            self.opening_on_account = false;
            self.opening_clear_date.clear();
            self.opening_clear_reference.clear();
            self.opening_clear_reason.clear();
        }
        if ui.button("Load existing opening balance").clicked() {
            if let Some(id) = self.opening_ledger {
                match self.store.opening_balance(company, id) {
                    Ok(Some(value)) => {
                        self.opening_date = value.date;
                        self.opening_debit = value.amount > 0;
                        self.opening_on_account = value.on_account;
                        self.opening_amount = format_amount(value.amount)
                            .trim_start_matches('-')
                            .to_owned();
                    }
                    Ok(None) => {
                        self.opening_date.clear();
                        self.opening_amount.clear();
                        self.opening_debit = true;
                        self.opening_on_account = false;
                        self.message = "No opening balance recorded for this ledger".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
        ui.horizontal(|ui| {
            ui.label("Date");
            ui.add(egui::TextEdit::singleline(&mut self.opening_date).hint_text("YYYY-MM-DD"));
            ui.label("Amount");
            ui.text_edit_singleline(&mut self.opening_amount);
            egui::ComboBox::from_id_salt("opening-side")
                .selected_text(if self.opening_debit {
                    "Debit"
                } else {
                    "Credit"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.opening_debit, true, "Debit");
                    ui.selectable_value(&mut self.opening_debit, false, "Credit");
                });
        });
        ui.checkbox(&mut self.opening_on_account, "On account")
            .on_hover_text("Enter the amount that is separate from named opening bills. Use the same opening date.");
        if ui.button("Save opening balance").clicked() {
            let result = (|| -> balance::Result<i64> {
                let ledger = self
                    .opening_ledger
                    .ok_or_else(|| balance::Error::Validation("Select a ledger".into()))?;
                let amount = parse_amount(&self.opening_amount)?;
                let amount = if self.opening_debit { amount } else { -amount };
                if self.opening_on_account {
                    self.store.set_opening_on_account_balance(
                        company,
                        ledger,
                        &self.opening_date,
                        amount,
                    )
                } else {
                    self.store
                        .set_opening_balance(company, ledger, &self.opening_date, amount)
                }
            })();
            match result {
                Ok(id) => self.message = format!("Opening balance saved (voucher {id})"),
                Err(e) => self.failure(e),
            }
        }
        ui.small("Opening balances must be non-zero. Clear an existing balance with a separate dated counter-entry; later openings cannot predate the last clearing.");
        if let Some(ledger) = self.opening_ledger {
            egui::CollapsingHeader::new("Clear opening balance")
                .id_salt(("clear-opening", ledger))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Clearing date");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.opening_clear_date)
                                .hint_text("YYYY-MM-DD"),
                        );
                        ui.label("Reference");
                        ui.text_edit_singleline(&mut self.opening_clear_reference);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Reason");
                        ui.text_edit_singleline(&mut self.opening_clear_reason);
                        if ui.button("Post opening balance clearing").clicked() {
                            match self.store.clear_opening_balance(
                                company,
                                ledger,
                                &self.opening_clear_date,
                                &self.opening_clear_reference,
                                &self.opening_clear_reason,
                            ) {
                                Ok(voucher) => {
                                    self.opening_date.clear();
                                    self.opening_amount.clear();
                                    self.opening_on_account = false;
                                    self.opening_clear_date.clear();
                                    self.opening_clear_reference.clear();
                                    self.opening_clear_reason.clear();
                                    self.message =
                                        format!("Opening balance cleared with voucher {voucher}");
                                }
                                Err(e) => self.failure(e),
                            }
                        }
                    });
                    match self.store.opening_clear_history(company, ledger) {
                        Ok(rows) => {
                            for entry in rows {
                                ui.label(format!(
                                    "{} — original voucher #{} · clearing #{}{} · {}",
                                    entry.date,
                                    entry.original_voucher_id,
                                    entry.clearing_voucher_id,
                                    if entry.on_account {
                                        " · On account"
                                    } else {
                                        ""
                                    },
                                    entry.reason
                                ));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                        }
                    }
                });
        }
        ui.separator();
        self.opening_bill_panel(ui, company, &ledgers);
        ui.separator();
        crate::theme::section(ui, "Ledger details");
        let dirty = self.profile_ledger.is_some() && self.ledger_profile != self.profile_original;
        pick_account(
            ui,
            "profile-ledger",
            "Ledger",
            &ledgers,
            &mut self.profile_selection,
            false,
        );
        if ui
            .add_enabled(
                !dirty && self.profile_selection.is_some(),
                egui::Button::new("Open details"),
            )
            .clicked()
        {
            if let Some(id) = self.profile_selection {
                match self.store.ledger_profile(company, id) {
                    Ok(profile) => {
                        self.profile_ledger = Some(id);
                        self.profile_original = profile.clone();
                        self.ledger_profile = profile;
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
        if let Some(id) = self.profile_ledger {
            let name = ledgers
                .iter()
                .find(|l| l.id == id)
                .map(|l| l.name.as_str())
                .unwrap_or("Unknown ledger");
            ui.label(format!("Editing details for {name}"));
            egui::Grid::new("ledger-details")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Mailing name");
                    ui.text_edit_singleline(&mut self.ledger_profile.mailing_name);
                    ui.end_row();
                    ui.label("Address");
                    ui.text_edit_multiline(&mut self.ledger_profile.address);
                    ui.end_row();
                    ui.label("Postal code");
                    ui.text_edit_singleline(&mut self.ledger_profile.postal_code);
                    ui.end_row();
                    ui.label("Region / state");
                    ui.text_edit_singleline(&mut self.ledger_profile.region);
                    ui.end_row();
                    ui.label("Country");
                    ui.text_edit_singleline(&mut self.ledger_profile.country);
                    ui.end_row();
                    ui.label("Email");
                    ui.text_edit_singleline(&mut self.ledger_profile.email);
                    ui.end_row();
                    ui.label("Email CC");
                    ui.text_edit_singleline(&mut self.ledger_profile.email_cc);
                    ui.end_row();
                    ui.label("Phone");
                    ui.text_edit_singleline(&mut self.ledger_profile.phone);
                    ui.end_row();
                    ui.label("Mobile");
                    ui.text_edit_singleline(&mut self.ledger_profile.mobile);
                    ui.end_row();
                    ui.label("Tax ID");
                    ui.text_edit_singleline(&mut self.ledger_profile.tax_id);
                    ui.end_row();
                    ui.label("Registration type");
                    ui.text_edit_singleline(&mut self.ledger_profile.tax_registration_type);
                    ui.end_row();
                });
            ui.horizontal(|ui| {
                if ui.button("Save details").clicked() {
                    match self
                        .store
                        .save_ledger_profile(company, id, &self.ledger_profile)
                    {
                        Ok(()) => {
                            self.profile_original = self.ledger_profile.clone();
                            self.message = "Ledger details saved".into();
                        }
                        Err(e) => self.failure(e),
                    }
                }
                if ui.button("Discard changes").clicked() {
                    self.ledger_profile = self.profile_original.clone();
                }
            });
            if self.ledger_profile != self.profile_original {
                ui.label("Unsaved ledger details");
            }
            egui::CollapsingHeader::new("Previous details")
                .id_salt(("profile-revisions", id))
                .show(ui, |ui| {
                    match self.store.ledger_profile_revisions(company, id) {
                        Ok(revisions) if revisions.is_empty() => {
                            ui.label("No previous versions.");
                        }
                        Ok(revisions) => {
                            for revision in revisions {
                                ui.label(format!("Replaced at {} UTC", revision.changed_at));
                                ui.label(format!(
                                    "{}\n{}\n{} {} {}\n{} {}\n{} {}\n{} {}",
                                    revision.profile.mailing_name,
                                    revision.profile.address,
                                    revision.profile.region,
                                    revision.profile.postal_code,
                                    revision.profile.country,
                                    revision.profile.email,
                                    revision.profile.email_cc,
                                    revision.profile.phone,
                                    revision.profile.mobile,
                                    revision.profile.tax_id,
                                    revision.profile.tax_registration_type
                                ));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                        }
                    }
                });
        }
        ui.separator();
        crate::theme::section(ui, "Party payment terms");
        ui.small("Defaults are optional. Credit limits are informational and do not prevent invoicing or payments.");
        let party_ledgers: Vec<_> = ledgers
            .iter()
            .filter(|ledger| matches!(ledger.category, Category::Asset | Category::Liability))
            .cloned()
            .collect();
        pick_account(
            ui,
            "terms-ledger",
            "Ledger",
            &party_ledgers,
            &mut self.terms_selection,
            false,
        );
        let terms_dirty = self.terms_ledger.is_some()
            && (self.terms_days != self.terms_original_days
                || self.terms_limit != self.terms_original_limit);
        if ui
            .add_enabled(
                !terms_dirty && self.terms_selection.is_some(),
                egui::Button::new("Open payment terms"),
            )
            .clicked()
        {
            if let Some(id) = self.terms_selection {
                match self.store.ledger_terms(company, id) {
                    Ok(terms) => {
                        self.terms_ledger = Some(id);
                        self.terms_days = terms
                            .credit_days
                            .map(|days| days.to_string())
                            .unwrap_or_default();
                        self.terms_limit =
                            terms.credit_limit.map(format_amount).unwrap_or_default();
                        self.terms_original_days = self.terms_days.clone();
                        self.terms_original_limit = self.terms_limit.clone();
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
        if let Some(id) = self.terms_ledger {
            let name = party_ledgers
                .iter()
                .find(|l| l.id == id)
                .map(|l| l.name.as_str())
                .unwrap_or("Unknown ledger");
            ui.label(format!("Editing payment terms for {name}"));
            ui.horizontal(|ui| {
                ui.label("Default credit days");
                ui.text_edit_singleline(&mut self.terms_days);
                ui.label("Informational credit limit");
                ui.text_edit_singleline(&mut self.terms_limit);
            });
            ui.horizontal(|ui| {
                if ui.button("Save payment terms").clicked() {
                    let result = (|| -> balance::Result<()> {
                        let credit_days = if self.terms_days.trim().is_empty() {
                            None
                        } else {
                            Some(self.terms_days.trim().parse::<u32>().map_err(|_| {
                                balance::Error::Validation(
                                    "Credit days must be a non-negative whole number".into(),
                                )
                            })?)
                        };
                        let credit_limit = if self.terms_limit.trim().is_empty() {
                            None
                        } else {
                            Some(parse_amount(&self.terms_limit)?)
                        };
                        self.store.save_ledger_terms(
                            company,
                            id,
                            &LedgerTerms {
                                credit_days,
                                credit_limit,
                            },
                        )?;
                        self.terms_days =
                            credit_days.map(|days| days.to_string()).unwrap_or_default();
                        self.terms_limit = credit_limit.map(format_amount).unwrap_or_default();
                        Ok(())
                    })();
                    match result {
                        Ok(()) => {
                            self.terms_original_days = self.terms_days.clone();
                            self.terms_original_limit = self.terms_limit.clone();
                            self.message = "Payment terms saved".into();
                        }
                        Err(e) => self.failure(e),
                    }
                }
                if ui.button("Discard payment term edits").clicked() {
                    self.terms_days = self.terms_original_days.clone();
                    self.terms_limit = self.terms_original_limit.clone();
                }
            });
            egui::CollapsingHeader::new("Payment term history")
                .id_salt(("term-history", id))
                .show(ui, |ui| {
                    match self.store.ledger_terms_history(company, id) {
                        Ok(rows) if rows.is_empty() => {
                            ui.label("No earlier versions.");
                        }
                        Ok(rows) => {
                            for row in rows {
                                ui.label(format!(
                                    "{} UTC — credit days: {} · limit: {}",
                                    row.changed_at,
                                    row.terms
                                        .credit_days
                                        .map(|v| v.to_string())
                                        .unwrap_or_else(|| "none".into()),
                                    row.terms
                                        .credit_limit
                                        .map(format_amount)
                                        .unwrap_or_else(|| "none".into())
                                ));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                        }
                    }
                });
        }
        ui.separator();
        crate::theme::section(ui, "Account groups");
        ui.horizontal(|ui| {
            ui.label("New group");
            ui.text_edit_singleline(&mut self.group_name);
            let parent_name = groups
                .iter()
                .find(|g| Some(g.id) == self.group_parent)
                .map(|g| g.name.as_str())
                .unwrap_or("No parent");
            egui::ComboBox::from_label("Parent")
                .selected_text(parent_name)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.group_parent, None, "No parent");
                    for group in &groups {
                        ui.selectable_value(&mut self.group_parent, Some(group.id), &group.name);
                    }
                });
            if self.group_parent.is_none() {
                egui::ComboBox::from_label("Category")
                    .selected_text(self.category.as_str())
                    .show_ui(ui, |ui| {
                        for c in Category::ALL {
                            ui.selectable_value(&mut self.category, c, c.as_str());
                        }
                    });
            }
        });
        if ui.button("Create group").clicked() {
            match self.store.create_group(
                company,
                &self.group_name,
                self.group_parent,
                self.category,
            ) {
                Ok(_) => {
                    self.group_name.clear();
                    self.message = "Account group created".into();
                }
                Err(e) => self.failure(e),
            }
        }
        for group in &groups {
            let parent = group
                .parent_id
                .and_then(|id| groups.iter().find(|g| g.id == id))
                .map(|g| g.name.as_str())
                .unwrap_or("Root");
            ui.label(format!(
                "{}  —  {}  ({})",
                group.name,
                parent,
                group.category.as_str()
            ));
        }
    }
    fn voucher_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(
            ui,
            match self.editing_voucher {
                Some(id) => format!("Edit voucher {id}"),
                None => format!("{} voucher", self.voucher_kind.as_str()),
            },
        );
        ui.label(
            "Enter at least two lines. Debits and credits must balance. Press Ctrl+Enter to save.",
        );
        egui::ComboBox::from_label("Type")
            .selected_text(self.voucher_kind.as_str())
            .show_ui(ui, |ui| {
                for kind in VoucherKind::ALL {
                    ui.selectable_value(&mut self.voucher_kind, kind, kind.as_str());
                }
            });
        ui.small("Type is a classification; inventory and tax are not calculated automatically.");
        let ledgers = match self.store.ledgers(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        ui.horizontal(|ui| {
            ui.label("Date (YYYY-MM-DD)");
            ui.text_edit_singleline(&mut self.date);
            ui.label("Reference / number");
            ui.text_edit_singleline(&mut self.voucher_reference);
        });
        let mut remove = None;
        for (index, line) in self.lines.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{}.", index + 1));
                egui::ComboBox::from_id_salt(("voucher-ledger", index))
                    .selected_text(
                        ledgers
                            .iter()
                            .find(|l| Some(l.id) == line.ledger)
                            .map(|l| l.name.as_str())
                            .unwrap_or("Select ledger"),
                    )
                    .show_ui(ui, |ui| {
                        for ledger in &ledgers {
                            ui.selectable_value(&mut line.ledger, Some(ledger.id), &ledger.name);
                        }
                    });
                egui::ComboBox::from_id_salt(("voucher-side", index))
                    .selected_text(if line.debit { "Debit" } else { "Credit" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut line.debit, true, "Debit");
                        ui.selectable_value(&mut line.debit, false, "Credit");
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut line.amount)
                        .hint_text("Amount")
                        .desired_width(110.0),
                );
                ui.checkbox(&mut line.is_party, "Party");
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            if self.lines.len() > 2 {
                self.lines.remove(index);
            } else {
                self.message = "A voucher requires at least two lines".into();
            }
        }
        if ui.button("Add line").clicked() {
            self.lines.push(VoucherLine::new(true));
        }
        ui.horizontal(|ui| {
            ui.label("Narration");
            ui.text_edit_singleline(&mut self.narration);
        });
        let submit = theme::primary_button(
            ui,
            if self.editing_voucher.is_some() {
                "Save changes"
            } else {
                "Post voucher"
            },
        )
        .clicked()
            || (!self.shell.go_to_is_open()
                && ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::Enter)));
        if self.editing_voucher.is_some() && ui.button("Cancel edit").clicked() {
            self.clear_voucher();
            self.message.clear();
        }
        let save_schedule = if self.editing_voucher.is_none()
            && self.voucher_kind == VoucherKind::Journal
        {
            ui.button("Save as monthly schedule")
                    .on_hover_text("The date becomes the first occurrence and the reference becomes the schedule name.")
                    .clicked()
        } else {
            false
        };
        if submit || save_schedule {
            let result = (|| -> balance::Result<i64> {
                let mut postings = Vec::with_capacity(self.lines.len());
                let mut party_lines = Vec::new();
                for (index, line) in self.lines.iter().enumerate() {
                    let ledger_id = line.ledger.ok_or_else(|| {
                        balance::Error::Validation("Select a ledger for each line".into())
                    })?;
                    let amount = parse_amount(&line.amount)?;
                    if amount == 0 {
                        return Err(balance::Error::Validation(
                            "Amounts must be greater than zero".into(),
                        ));
                    }
                    postings.push(Posting {
                        ledger_id,
                        amount: if line.debit { amount } else { -amount },
                    });
                    if line.is_party {
                        party_lines.push(index);
                    }
                }
                let draft = VoucherDraft {
                    kind: self.voucher_kind,
                    reference: &self.voucher_reference,
                    date: &self.date,
                    narration: &self.narration,
                    postings: &postings,
                    party_lines: &party_lines,
                };
                if save_schedule {
                    self.store.create_monthly_journal_from_draft(company, draft)
                } else if let Some(id) = self.editing_voucher {
                    self.store.update_voucher_draft(company, id, draft)?;
                    Ok(id)
                } else {
                    self.store.post_voucher_draft(company, draft)
                }
            })();
            match result {
                Ok(id) if save_schedule => {
                    self.clear_voucher();
                    self.open_recurring_journal(id);
                    self.message = "Monthly schedule saved".into();
                }
                Ok(id) => {
                    self.message = format!("Voucher {id} saved");
                    self.clear_voucher();
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        crate::theme::section(ui, "Voucher history");
        if let Some(target) = self.voucher_reversal_target {
            ui.label(format!(
                "Reverse voucher #{target} with a separate journal entry"
            ));
            ui.horizontal(|ui| {
                ui.label("Date (YYYY-MM-DD)");
                ui.text_edit_singleline(&mut self.voucher_reversal_date);
                ui.label("Reference");
                ui.text_edit_singleline(&mut self.voucher_reversal_reference);
            });
            ui.horizontal(|ui| {
                ui.label("Reason");
                ui.text_edit_singleline(&mut self.voucher_reversal_reason);
                if ui
                    .add_enabled(
                        self.editing_voucher.is_none(),
                        egui::Button::new("Post voucher reversal"),
                    )
                    .clicked()
                {
                    match self.store.reverse_voucher(
                        company,
                        target,
                        &self.voucher_reversal_date,
                        &self.voucher_reversal_reference,
                        &self.voucher_reversal_reason,
                    ) {
                        Ok(id) => {
                            self.voucher_reversal_target = None;
                            self.voucher_reversal_date.clear();
                            self.voucher_reversal_reference.clear();
                            self.voucher_reversal_reason.clear();
                            self.message = format!("Reversal voucher {id} posted");
                        }
                        Err(e) => self.failure(e),
                    }
                }
                if ui.button("Cancel reversal").clicked() {
                    self.voucher_reversal_target = None;
                }
            });
            ui.small("The counter-entry offsets the original postings and restores linked bill balances or available credit from the reversal date.");
        }
        match self.store.vouchers(company) {
            Ok(vouchers) => {
                for voucher in vouchers {
                    let invoice_backed = self
                        .store
                        .is_invoice_voucher(company, voucher.id)
                        .unwrap_or(true);
                    let reversal = match self.store.voucher_reversal(company, voucher.id) {
                        Ok(value) => value,
                        Err(e) => {
                            self.failure(e);
                            return;
                        }
                    };
                    let protected = invoice_backed || reversal.is_some();
                    let bill_note = self.store.is_bill_note(company, voucher.id).unwrap_or(true);
                    let named_refund = self
                        .store
                        .is_named_credit_refund(company, voucher.id)
                        .unwrap_or(true);
                    let foreign = self
                        .store
                        .is_foreign_voucher(company, voucher.id)
                        .unwrap_or(true);
                    let recurring = self
                        .store
                        .is_monthly_journal_voucher(company, voucher.id)
                        .unwrap_or(true);
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{}  #{}  {}",
                            voucher.date, voucher.id, voucher.narration
                        ));
                        ui.label(voucher.kind.as_str());
                        if !voucher.reference.is_empty() {
                            ui.label(format!("Ref: {}", voucher.reference));
                        }
                        if ui
                            .add_enabled(
                                !protected && !bill_note && !named_refund && !foreign && !recurring,
                                egui::Button::new("Edit"),
                            )
                            .clicked()
                        {
                            self.editing_voucher = Some(voucher.id);
                            self.voucher_kind = voucher.kind;
                            self.voucher_reference = voucher.reference.clone();
                            self.date = voucher.date.clone();
                            self.narration = voucher.narration.clone();
                            self.lines = voucher
                                .postings
                                .iter()
                                .enumerate()
                                .map(|(index, p)| VoucherLine {
                                    ledger: Some(p.ledger_id),
                                    debit: p.amount > 0,
                                    amount: format_amount(p.amount)
                                        .trim_start_matches('-')
                                        .to_owned(),
                                    is_party: voucher.party_lines.contains(&index),
                                })
                                .collect();
                            self.message.clear();
                        }
                        if ui
                            .add_enabled(
                                !protected && self.editing_voucher.is_none(),
                                egui::Button::new("Select for reversal"),
                            )
                            .clicked()
                        {
                            self.voucher_reversal_target = Some(voucher.id);
                        }
                        if foreign {
                            ui.small("Foreign amounts are in Currencies.");
                        } else if recurring {
                            ui.small("Monthly journal. Use a reversal to correct it.");
                        } else if bill_note {
                            ui.small("Bill-linked note. Use a reversal to correct it.");
                        } else if named_refund && !protected {
                            ui.small("Named credit refund. Use a reversal to correct it.");
                        } else if protected {
                            ui.small("Linked or reversed postings cannot be edited here.");
                        }
                    });
                    if let Some(reversal) = reversal {
                        ui.small(format!(
                            "Original #{} · reversal #{} on {} · {}",
                            reversal.original_voucher_id,
                            reversal.reversal_voucher_id,
                            reversal.date,
                            reversal.reason
                        ));
                    }
                    for (index, p) in voucher.postings.iter().enumerate() {
                        let name = ledgers
                            .iter()
                            .find(|l| l.id == p.ledger_id)
                            .map(|l| l.name.as_str())
                            .unwrap_or("Unknown ledger");
                        ui.label(format!(
                            "  {}{}  {}  {}",
                            name,
                            if voucher.party_lines.contains(&index) {
                                " (party)"
                            } else {
                                ""
                            },
                            if p.amount > 0 { "Dr" } else { "Cr" },
                            format_amount(p.amount).trim_start_matches('-')
                        ));
                    }
                    egui::CollapsingHeader::new("Previous versions")
                        .id_salt(("voucher-revisions", voucher.id))
                        .show(ui, |ui| {
                            match self.store.voucher_revisions(company, voucher.id) {
                                Ok(revisions) if revisions.is_empty() => {
                                    ui.label("No previous versions.");
                                }
                                Ok(revisions) => {
                                    for revision in revisions {
                                        ui.label(format!(
                                            "Replaced at {} UTC • {} • {} • {} • {}",
                                            revision.changed_at,
                                            revision.voucher.date,
                                            revision.voucher.kind.as_str(),
                                            revision.voucher.reference,
                                            revision.voucher.narration
                                        ));
                                        for (index, p) in
                                            revision.voucher.postings.iter().enumerate()
                                        {
                                            let name = ledgers
                                                .iter()
                                                .find(|l| l.id == p.ledger_id)
                                                .map(|l| l.name.as_str())
                                                .unwrap_or("Unknown ledger");
                                            ui.label(format!(
                                                "  {}{}  {}  {}",
                                                name,
                                                if revision.voucher.party_lines.contains(&index) {
                                                    " (party)"
                                                } else {
                                                    ""
                                                },
                                                if p.amount > 0 { "Dr" } else { "Cr" },
                                                format_amount(p.amount).trim_start_matches('-')
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                                }
                            }
                        });
                }
            }
            Err(e) => self.failure(e),
        }
    }
    fn reports_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Financial reports");
        self.report_export_path_field(ui);
        ui.label("Dates are inclusive. Leave a field blank for an open boundary; an empty end date includes future-dated entries.");
        ui.horizontal(|ui| {
            ui.label("Profit and loss from");
            ui.add(
                egui::TextEdit::singleline(&mut self.report_from)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            ui.label("To / as of");
            ui.add(
                egui::TextEdit::singleline(&mut self.report_to)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
        });
        let from = if self.report_from.trim().is_empty() {
            None
        } else {
            Some(self.report_from.trim())
        };
        let to = if self.report_to.trim().is_empty() {
            None
        } else {
            Some(self.report_to.trim())
        };
        let report = match self.store.financial_report(company, from, to) {
            Ok(v) => v,
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                return;
            }
        };
        crate::theme::section(ui, "Trial balance");
        ui.label("As of the end date; debit balances are positive, credits negative.");
        if ui.button("Export trial balance as CSV").clicked() {
            match self
                .store
                .export_trial_balance_csv(company, to, self.report_export_path.trim())
            {
                Ok(()) => self.message = "Trial balance exported".into(),
                Err(e) => self.message = e.to_string(),
            }
        }
        egui::Grid::new("trial_balance")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Ledger");
                ui.strong("Category");
                ui.strong("Balance");
                ui.end_row();
                for b in &report.trial_balance {
                    ui.label(&b.ledger.name);
                    ui.label(b.ledger.category.as_str());
                    ui.label(format_amount(b.amount));
                    ui.end_row();
                }
            });
        let debit: i128 = report
            .trial_balance
            .iter()
            .map(|balance| i128::from(balance.amount).max(0))
            .sum();
        let credit: i128 = report
            .trial_balance
            .iter()
            .map(|balance| (-i128::from(balance.amount)).max(0))
            .sum();
        ui.label(format!(
            "Total debits: {}    Total credits: {}    Difference: {}",
            money(debit),
            money(credit),
            money(debit - credit)
        ));
        if debit != credit {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Trial balance is out of balance; review the underlying records.",
            );
        }
        crate::theme::section(ui, "Trial balance by account group");
        ui.small("Each group total includes its descendants. Do not add parent and child rows together. Balances are grouped by each ledger's current group.");
        if ui.button("Export grouped trial balance as CSV").clicked() {
            match self.store.export_group_trial_balance_csv(
                company,
                to,
                self.report_export_path.trim(),
            ) {
                Ok(()) => self.message = "Grouped trial balance exported".into(),
                Err(e) => self.message = e.to_string(),
            }
        }
        match self.store.grouped_trial_balance(company, to) {
            Ok(grouped) => {
                ui.label(format!(
                    "Total debit: {}    Total credit: {}",
                    money(grouped.debit),
                    money(grouped.credit)
                ));
                egui::Grid::new("group-trial-balance")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in [
                            "Group",
                            "Category",
                            "Direct debit",
                            "Direct credit",
                            "Including subgroups debit",
                            "Including subgroups credit",
                        ] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for group in grouped
                            .groups
                            .iter()
                            .filter(|g| g.total_debit != 0 || g.total_credit != 0)
                        {
                            ui.label(group.path.join(" / "));
                            ui.label(group.group.category.as_str());
                            ui.label(money(group.direct_debit));
                            ui.label(money(group.direct_credit));
                            ui.label(money(group.total_debit));
                            ui.label(money(group.total_credit));
                            ui.end_row();
                        }
                    });
                if !grouped.ungrouped.is_empty() {
                    ui.label("Ungrouped ledgers:");
                    for balance in grouped.ungrouped {
                        ui.label(format!(
                            "{}: {}",
                            balance.ledger.name,
                            format_amount(balance.amount)
                        ));
                    }
                }
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
        ui.separator();
        crate::theme::section(ui, "Profit and loss");
        ui.label(format!(
            "Income: {}    Expenses: {}    Net profit: {}",
            money(report.income),
            money(report.expenses),
            money(report.profit)
        ));
        crate::theme::section(ui, "Balance sheet");
        ui.label(format!(
            "Assets: {}    Liabilities: {}    Equity including profit: {}",
            money(report.assets),
            money(report.liabilities),
            money(report.equity)
        ));
        ui.small("The balance sheet includes cumulative profit through the end date and recorded opening balances. Automatic tax and statutory adjustments are not implemented.");
    }
    fn register_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Ledger activity");
        self.report_export_path_field(ui);
        let ledgers = match self.store.ledgers(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let name = ledgers
            .iter()
            .find(|l| Some(l.id) == self.register_ledger)
            .map(|l| l.name.as_str())
            .unwrap_or("Select ledger");
        egui::ComboBox::from_label("Ledger")
            .selected_text(name)
            .show_ui(ui, |ui| {
                for ledger in &ledgers {
                    ui.selectable_value(&mut self.register_ledger, Some(ledger.id), &ledger.name);
                }
            });
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(
                egui::TextEdit::singleline(&mut self.register_from)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            ui.label("To");
            ui.add(
                egui::TextEdit::singleline(&mut self.register_to)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
        });
        let Some(ledger) = self.register_ledger else {
            ui.label("Select a ledger to view its entries.");
            return;
        };
        let from = if self.register_from.trim().is_empty() {
            None
        } else {
            Some(self.register_from.trim())
        };
        let to = if self.register_to.trim().is_empty() {
            None
        } else {
            Some(self.register_to.trim())
        };
        let register = match self.store.ledger_register(company, ledger, from, to) {
            Ok(v) => v,
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                return;
            }
        };
        let export_csv = ui.button("Export ledger activity as CSV").clicked();
        let export_html = ui
            .horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.ledger_statement_path)
                        .hint_text("Absolute path to a new .html file")
                        .desired_width(350.0),
                );
                ui.button("Export printable statement").clicked()
            })
            .inner;
        let export = if export_csv {
            Some(
                self.store
                    .export_ledger_register_csv(
                        company,
                        ledger,
                        from,
                        to,
                        self.report_export_path.trim(),
                    )
                    .map(|()| "Ledger report exported"),
            )
        } else if export_html {
            Some(
                self.store
                    .export_ledger_statement_html(
                        company,
                        ledger,
                        from,
                        to,
                        self.ledger_statement_path.trim(),
                    )
                    .map(|()| "Ledger statement exported"),
            )
        } else {
            None
        };
        if let Some(result) = export {
            match result {
                Ok(text) => self.message = text.into(),
                Err(e) => self.failure(e),
            }
        }
        ui.label(format!(
            "Opening balance: {}",
            money(register.opening_balance)
        ));
        egui::Grid::new("ledger-register")
            .striped(true)
            .show(ui, |ui| {
                for h in ["Date", "Voucher", "Narration", "Debit", "Credit", "Balance"] {
                    ui.strong(h);
                }
                ui.end_row();
                for entry in register.entries {
                    ui.label(entry.date);
                    ui.label(entry.voucher_id.to_string());
                    ui.label(entry.narration);
                    ui.label(if entry.amount > 0 {
                        format_amount(entry.amount)
                    } else {
                        "—".into()
                    });
                    ui.label(if entry.amount < 0 {
                        format_amount(entry.amount)
                            .trim_start_matches('-')
                            .to_string()
                    } else {
                        "—".into()
                    });
                    ui.label(money(entry.running_balance));
                    ui.end_row();
                }
            });
        ui.label(format!(
            "Closing balance: {}",
            money(register.closing_balance)
        ));
        ui.small(
            "Debit balances are positive; credit balances are negative. Boundaries are inclusive.",
        );
    }
    fn stock_page(&mut self, ui: &mut egui::Ui, company: i64) {
        egui::CollapsingHeader::new("Negative stock control").show(ui, |ui| {
            if let Err(error) = self.negative_stock_view(ui, company) {
                self.failure(error);
            }
        });
        let locations = match self.store.stock_locations(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let main_location = locations.iter().find(|l| l.name == "Main").map(|l| l.id);
        if self.adjustment_location.is_none() {
            self.adjustment_location = main_location;
        }
        if self.transfer_from.is_none() {
            self.transfer_from = main_location;
        }
        let units = match self.store.stock_units(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let groups = match self.store.stock_groups(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        if self.stock_creation_group.is_none() {
            self.stock_creation_group = groups
                .iter()
                .find(|g| g.parent_id.is_none() && g.name == "Primary")
                .map(|g| g.id);
        }
        if self.stock_group_parent.is_none() {
            self.stock_group_parent = groups
                .iter()
                .find(|g| g.parent_id.is_none() && g.name == "Primary")
                .map(|g| g.id);
        }
        crate::theme::section(ui, "Units and stock groups");
        ui.horizontal(|ui| {
            ui.label("Unit symbol");
            ui.text_edit_singleline(&mut self.unit_symbol);
            ui.label("Formal name");
            ui.text_edit_singleline(&mut self.unit_formal_name);
            ui.label("Decimal places");
            ui.add(egui::DragValue::new(&mut self.unit_decimals).range(0..=3));
            if ui.button("Create unit").clicked() {
                match self.store.create_stock_unit(
                    company,
                    &self.unit_symbol,
                    &self.unit_formal_name,
                    self.unit_decimals,
                ) {
                    Ok(id) => {
                        self.stock_creation_unit = Some(id);
                        self.unit_symbol.clear();
                        self.unit_formal_name.clear();
                        self.message = "Unit created".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        egui::CollapsingHeader::new("Create compound unit").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                pick_unit(
                    ui,
                    "compound-outer-unit",
                    "Outer unit",
                    &units,
                    true,
                    &mut self.compound_unit.primary,
                );
                pick_unit(
                    ui,
                    "compound-inner-unit",
                    "Contained unit",
                    &units,
                    false,
                    &mut self.compound_unit.tail,
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Contained units per outer unit");
                ui.add(
                    egui::TextEdit::singleline(&mut self.compound_unit.conversion)
                        .desired_width(80.0),
                );
                ui.label("Unit symbol");
                ui.add(
                    egui::TextEdit::singleline(&mut self.compound_unit.symbol)
                        .hint_text("Generate automatically"),
                );
                if ui.button("Create compound unit").clicked() {
                    let result = match (self.compound_unit.primary, self.compound_unit.tail) {
                        (Some(primary), Some(tail)) => self.store.create_compound_stock_unit(
                            company,
                            &self.compound_unit.symbol,
                            primary,
                            tail,
                            &self.compound_unit.conversion,
                        ),
                        _ => Err(balance::Error::Validation(
                            "Select both units for the conversion".into(),
                        )),
                    };
                    match result {
                        Ok(id) => {
                            self.stock_creation_unit = Some(id);
                            self.compound_unit = CompoundUnitForm::default();
                            self.message = "Compound unit created".into();
                        }
                        Err(error) => self.failure(error),
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Stock group");
            ui.text_edit_singleline(&mut self.stock_group_name);
            let parent = groups
                .iter()
                .find(|g| Some(g.id) == self.stock_group_parent)
                .map(|g| g.name.as_str())
                .unwrap_or("Select parent");
            egui::ComboBox::from_id_salt("stock-group-parent")
                .selected_text(parent)
                .show_ui(ui, |ui| {
                    for group in &groups {
                        ui.selectable_value(
                            &mut self.stock_group_parent,
                            Some(group.id),
                            &group.name,
                        );
                    }
                });
            if ui.button("Create stock group").clicked() {
                let result = match self.stock_group_parent {
                    Some(id) => self
                        .store
                        .create_stock_group(company, &self.stock_group_name, id),
                    None => Err(balance::Error::Validation(
                        "Select a parent stock group".into(),
                    )),
                };
                match result {
                    Ok(id) => {
                        self.stock_creation_group = Some(id);
                        self.stock_group_name.clear();
                        self.message = "Stock group created".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        ui.separator();
        crate::theme::section(ui, "Stock locations");
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.stock_location_name);
            ui.label("Parent");
            let parent = locations
                .iter()
                .find(|location| Some(location.id) == self.stock_location_parent)
                .map(|location| location.name.as_str())
                .unwrap_or("Top level");
            egui::ComboBox::from_id_salt("stock-location-parent")
                .selected_text(parent)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.stock_location_parent, None, "Top level");
                    for location in &locations {
                        ui.selectable_value(
                            &mut self.stock_location_parent,
                            Some(location.id),
                            &location.name,
                        );
                    }
                });
            if ui.button("Create location").clicked() {
                match self.store.create_stock_location_under(
                    company,
                    &self.stock_location_name,
                    self.stock_location_parent,
                ) {
                    Ok(id) => {
                        self.stock_location_name.clear();
                        self.transfer_to = Some(id);
                        self.message = "Stock location created".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        ui.collapsing("Location list", |ui| {
            for location in &locations {
                let parent = locations
                    .iter()
                    .find(|parent| Some(parent.id) == location.parent_id);
                ui.horizontal(|ui| {
                    ui.label(&location.name);
                    if let Some(parent) = parent {
                        ui.small(format!("in {}", parent.name));
                    }
                });
            }
        });
        ui.separator();
        crate::theme::section(ui, "Stock items");
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.stock_name);
            ui.label("Unit");
            let unit = units
                .iter()
                .find(|u| Some(u.id) == self.stock_creation_unit)
                .map(|u| u.symbol.as_str())
                .unwrap_or("Select unit");
            egui::ComboBox::from_id_salt("stock-create-unit")
                .selected_text(unit)
                .show_ui(ui, |ui| {
                    for value in &units {
                        ui.selectable_value(
                            &mut self.stock_creation_unit,
                            Some(value.id),
                            &value.symbol,
                        );
                    }
                });
            ui.label("Group");
            let group = groups
                .iter()
                .find(|g| Some(g.id) == self.stock_creation_group)
                .map(|g| g.name.as_str())
                .unwrap_or("Select group");
            egui::ComboBox::from_id_salt("stock-create-group")
                .selected_text(group)
                .show_ui(ui, |ui| {
                    for value in &groups {
                        ui.selectable_value(
                            &mut self.stock_creation_group,
                            Some(value.id),
                            &value.name,
                        );
                    }
                });
            if ui.button("Create item").clicked() {
                let result = match (self.stock_creation_unit, self.stock_creation_group) {
                    (Some(unit), Some(group)) => self.store.create_stock_item_with_description(
                        company,
                        &self.stock_name,
                        unit,
                        group,
                        &self.stock_description,
                    ),
                    _ => Err(balance::Error::Validation(
                        "Select a unit and stock group".into(),
                    )),
                };
                match result {
                    Ok(_) => {
                        self.stock_name.clear();
                        self.stock_description.clear();
                        self.message = "Stock item created".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Item description");
            ui.text_edit_singleline(&mut self.stock_description);
        });
        let items = match self.store.stock_items(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        ui.separator();
        crate::theme::section(ui, "Stock price lists");
        ui.small("Prices are per base unit. A later entry for the same item and effective date replaces the earlier value for lookup; both remain in history. Invoice prices are saved independently.");
        ui.horizontal(|ui| {
            ui.label("New price list");
            ui.text_edit_singleline(&mut self.price_list_name);
            if ui.button("Create price list").clicked() {
                match self.store.create_price_list(company, &self.price_list_name) {
                    Ok(id) => {
                        self.price_list = Some(id);
                        self.price_list_name.clear();
                        self.message = "Price list created".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        let lists = match self.store.price_lists(company) {
            Ok(lists) => lists,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        pick_price_list(ui, "stock-price-list", &lists, &mut self.price_list);
        pick_stock(ui, "stock-price-item", "Item", &items, &mut self.price_item);
        ui.horizontal(|ui| {
            ui.label("Effective date");
            ui.add(egui::TextEdit::singleline(&mut self.price_date).hint_text("YYYY-MM-DD"));
            ui.label("Price");
            ui.add(egui::TextEdit::singleline(&mut self.price_amount).hint_text("0.00"));
            ui.label("Reason");
            ui.text_edit_singleline(&mut self.price_reason);
            if ui.button("Record price").clicked() {
                let result = (|| -> balance::Result<i64> {
                    self.store.record_stock_price(
                        company,
                        self.price_list.ok_or_else(|| {
                            balance::Error::Validation("Select a price list".into())
                        })?,
                        self.price_item
                            .ok_or_else(|| balance::Error::Validation("Select an item".into()))?,
                        &self.price_date,
                        parse_amount(&self.price_amount)?,
                        &self.price_reason,
                    )
                })();
                match result {
                    Ok(_) => {
                        self.price_amount.clear();
                        self.price_reason.clear();
                        self.message = "Price recorded".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        if let Some(list) = self.price_list {
            egui::CollapsingHeader::new("Price history")
                .id_salt(("price-history", list))
                .show(ui, |ui| {
                    match self.store.stock_price_history(company, list) {
                        Ok(rows) if rows.is_empty() => {
                            ui.label("No prices recorded.");
                        }
                        Ok(rows) => {
                            for entry in rows {
                                ui.label(format!(
                                    "{} — {}: {} per {} · {} (recorded {} UTC)",
                                    entry.effective_date,
                                    entry.item,
                                    format_amount(entry.unit_price),
                                    entry.unit,
                                    entry.reason,
                                    entry.recorded_at
                                ));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                        }
                    }
                });
        }
        ui.separator();
        crate::theme::section(ui, "Reorder settings");
        ui.small("Warnings compare company-wide on-hand quantities with current rules; they do not include future orders or create purchase orders.");
        pick_stock(
            ui,
            "reorder-item",
            "Item",
            &items,
            &mut self.reorder_selection,
        );
        if ui
            .add_enabled(
                self.reorder_editing.is_none() && self.reorder_selection.is_some(),
                egui::Button::new("Open reorder settings"),
            )
            .clicked()
        {
            if let Some(item) = self.reorder_selection {
                match self.store.reorder_rule(company, item) {
                    Ok(rule) => {
                        self.reorder_editing = Some(item);
                        self.reorder_enabled = rule.as_ref().is_none_or(|value| value.enabled);
                        self.reorder_level = rule
                            .as_ref()
                            .map(|value| format_quantity(i128::from(value.level)))
                            .unwrap_or_default();
                        self.reorder_quantity = rule
                            .as_ref()
                            .map(|value| format_quantity(i128::from(value.quantity)))
                            .unwrap_or_default();
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
        if let Some(item) = self.reorder_editing {
            let name = items
                .iter()
                .find(|value| value.id == item)
                .map(|value| value.name.as_str())
                .unwrap_or("Unknown item");
            ui.label(format!("Editing reorder settings for {name}"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.reorder_enabled, "Enable warning");
                ui.label("Reorder at or below");
                ui.text_edit_singleline(&mut self.reorder_level);
                ui.label("Suggested quantity");
                ui.text_edit_singleline(&mut self.reorder_quantity);
            });
            ui.horizontal(|ui| {
                if ui.button("Save reorder settings").clicked() {
                    let result = (|| -> balance::Result<()> {
                        let rule = ReorderRule {
                            enabled: self.reorder_enabled,
                            level: parse_quantity(&self.reorder_level)?,
                            quantity: parse_quantity(&self.reorder_quantity)?,
                        };
                        self.store.save_reorder_rule(company, item, &rule)
                    })();
                    match result {
                        Ok(()) => {
                            self.reorder_editing = None;
                            self.message = "Reorder settings saved".into();
                        }
                        Err(e) => self.failure(e),
                    }
                }
                if ui.button("Cancel reorder settings").clicked() {
                    self.reorder_editing = None;
                }
            });
            egui::CollapsingHeader::new("Reorder setting history")
                .id_salt(("reorder-history", item))
                .show(ui, |ui| {
                    match self.store.reorder_rule_history(company, item) {
                        Ok(rows) if rows.is_empty() => {
                            ui.label("No earlier versions.");
                        }
                        Ok(rows) => {
                            for row in rows {
                                ui.label(format!(
                                    "{} UTC — {} · level {} · quantity {}",
                                    row.changed_at,
                                    if row.rule.enabled {
                                        "enabled"
                                    } else {
                                        "disabled"
                                    },
                                    format_quantity(i128::from(row.rule.level)),
                                    format_quantity(i128::from(row.rule.quantity))
                                ));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                        }
                    }
                });
        }
        ui.separator();
        crate::theme::section(ui, "Stock batches");
        pick_stock(ui, "batch-item", "Item", &items, &mut self.batch_item);
        ui.horizontal(|ui| {
            ui.label("Batch name");
            ui.text_edit_singleline(&mut self.batch_name);
            ui.label("Manufactured (optional)");
            ui.add(
                egui::TextEdit::singleline(&mut self.batch_manufactured).hint_text("YYYY-MM-DD"),
            );
            ui.label("Expires (optional)");
            ui.add(egui::TextEdit::singleline(&mut self.batch_expiry).hint_text("YYYY-MM-DD"));
        });
        if ui.button("Create batch").clicked() {
            let result = match self.batch_item {
                Some(item) => self.store.create_stock_batch(
                    company,
                    item,
                    &self.batch_name,
                    (!self.batch_manufactured.trim().is_empty())
                        .then_some(self.batch_manufactured.trim()),
                    (!self.batch_expiry.trim().is_empty()).then_some(self.batch_expiry.trim()),
                ),
                None => Err(balance::Error::Validation("Select an item".into())),
            };
            match result {
                Ok(_) => {
                    self.batch_name.clear();
                    self.batch_manufactured.clear();
                    self.batch_expiry.clear();
                    self.message = "Stock batch created".into();
                }
                Err(e) => self.failure(e),
            }
        }
        if let Some(item) = self.batch_item {
            ui.small("Batch quantities use the location and as-of filters below. They include movements assigned to the selected batch.");
            match self.store.stock_batches(company, item) {
                Ok(batches) => {
                    for batch in batches {
                        let as_of = (!self.stock_as_of.trim().is_empty())
                            .then_some(self.stock_as_of.trim());
                        match self.store.stock_batch_quantity_in_scope(
                            company,
                            item,
                            batch.id,
                            self.stock_balance_location,
                            self.stock_balance_descendants,
                            as_of,
                        ) {
                            Ok(qty) => {
                                let expired = as_of
                                    .and_then(|date| {
                                        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
                                    })
                                    .zip(batch.expires_on.as_deref().and_then(|date| {
                                        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
                                    }))
                                    .is_some_and(|(date, expiry)| date > expiry);
                                let text = format!(
                                    "{} — quantity {} — manufactured {} — expires {}",
                                    batch.name,
                                    format_quantity(qty),
                                    batch.manufactured_on.as_deref().unwrap_or("—"),
                                    batch.expires_on.as_deref().unwrap_or("—")
                                );
                                if expired {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        format!("{text} — expired as of selected date"),
                                    );
                                } else {
                                    ui.label(text);
                                }
                            }
                            Err(e) => {
                                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                            }
                        }
                    }
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        crate::theme::section(ui, "Edit stock item");
        if self.stock_editing.is_none() {
            pick_stock(
                ui,
                "edit-stock-item",
                "Item",
                &items,
                &mut self.stock_edit_selection,
            );
            if ui.button("Edit selected item").clicked() {
                if let Some(item) = items
                    .iter()
                    .find(|item| Some(item.id) == self.stock_edit_selection)
                {
                    self.stock_editing = Some(item.id);
                    self.stock_edit_name = item.name.clone();
                    self.stock_edit_group = Some(item.group_id);
                    self.stock_edit_description = item.description.clone();
                } else {
                    self.message = "Select a stock item".into();
                }
            }
        }
        if let Some(item_id) = self.stock_editing {
            if let Some(item) = items.iter().find(|item| item.id == item_id) {
                ui.label(format!("Stock quantity unit {}", item.unit));
                if let Some(unit) = units
                    .iter()
                    .find(|unit| unit.id == item.unit_id && unit.compound.is_some())
                {
                    ui.label(format!("Compound unit {}", unit.symbol));
                }
            }
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut self.stock_edit_name);
                ui.label("Group");
                let group = groups
                    .iter()
                    .find(|g| Some(g.id) == self.stock_edit_group)
                    .map(|g| g.name.as_str())
                    .unwrap_or("Select group");
                egui::ComboBox::from_id_salt("stock-edit-group")
                    .selected_text(group)
                    .show_ui(ui, |ui| {
                        for value in &groups {
                            ui.selectable_value(
                                &mut self.stock_edit_group,
                                Some(value.id),
                                &value.name,
                            );
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.label("Description");
                ui.text_edit_singleline(&mut self.stock_edit_description);
            });
            ui.horizontal(|ui| {
                if ui.button("Save item").clicked() {
                    let result = match self.stock_edit_group {
                        Some(group) => self.store.update_stock_item(
                            company,
                            item_id,
                            &self.stock_edit_name,
                            group,
                            &self.stock_edit_description,
                        ),
                        None => Err(balance::Error::Validation("Select a stock group".into())),
                    };
                    match result {
                        Ok(()) => {
                            self.stock_editing = None;
                            self.message = "Stock item saved".into();
                        }
                        Err(e) => self.failure(e),
                    }
                }
                if ui.button("Discard item edits").clicked() {
                    self.stock_editing = None;
                }
            });
            match self.store.stock_item_revisions(company, item_id) {
                Ok(revisions) => {
                    egui::CollapsingHeader::new("Prior item versions").show(ui, |ui| {
                        if revisions.is_empty() {
                            ui.label("No prior versions.");
                        }
                        for revision in revisions {
                            ui.label(format!(
                                "{} — {} / {} / {}",
                                revision.changed_at,
                                revision.name,
                                revision.group,
                                revision.description
                            ));
                        }
                    });
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        crate::theme::section(ui, "Quantity adjustment");
        ui.label("Adjustments change quantities only; create any accounting entry separately.");
        let previous_adjustment_item = self.adjustment_item;
        pick_stock(
            ui,
            "adjustment-item",
            "Item",
            &items,
            &mut self.adjustment_item,
        );
        if previous_adjustment_item != self.adjustment_item {
            self.adjustment_batch = None;
        }
        if let Some(item) = self.adjustment_item {
            match self.store.stock_batches(company, item) {
                Ok(batches) => {
                    pick_batch(ui, "adjustment-batch", &batches, &mut self.adjustment_batch)
                }
                Err(e) => self.failure(e),
            }
        }
        pick_location(
            ui,
            "adjustment-location",
            "Location",
            &locations,
            &mut self.adjustment_location,
            false,
        );
        ui.horizontal(|ui| {
            ui.label("Date");
            ui.add(egui::TextEdit::singleline(&mut self.adjustment_date).hint_text("YYYY-MM-DD"));
            ui.label("Quantity");
            ui.text_edit_singleline(&mut self.adjustment_quantity);
            egui::ComboBox::from_id_salt("adjustment-direction")
                .selected_text(if self.adjustment_increase {
                    "Increase"
                } else {
                    "Decrease"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.adjustment_increase, true, "Increase");
                    ui.selectable_value(&mut self.adjustment_increase, false, "Decrease");
                });
        });
        ui.horizontal(|ui| {
            ui.label("Reason");
            ui.text_edit_singleline(&mut self.adjustment_reason);
        });
        if ui.button("Save adjustment").clicked() {
            let result = (|| -> balance::Result<i64> {
                let item = self
                    .adjustment_item
                    .ok_or_else(|| balance::Error::Validation("Select a stock item".into()))?;
                let qty = parse_quantity(&self.adjustment_quantity)?;
                if qty == 0 {
                    return Err(balance::Error::Validation(
                        "Quantity must be greater than zero".into(),
                    ));
                }
                self.store.adjust_stock_batch(
                    company,
                    item,
                    self.adjustment_location.ok_or_else(|| {
                        balance::Error::Validation("Select a stock location".into())
                    })?,
                    self.adjustment_batch,
                    &self.adjustment_date,
                    if self.adjustment_increase { qty } else { -qty },
                    &self.adjustment_reason,
                )
            })();
            match result {
                Ok(_) => {
                    self.adjustment_quantity.clear();
                    self.adjustment_reason.clear();
                    self.message = "Stock adjustment saved".into();
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        crate::theme::section(ui, "Transfer stock");
        ui.checkbox(
            &mut self.stock_transit_screen.separate_dispatch,
            "Record dispatch and receipt separately",
        );
        let previous_transfer_item = self.transfer_item;
        pick_stock(ui, "transfer-item", "Item", &items, &mut self.transfer_item);
        if previous_transfer_item != self.transfer_item {
            self.transfer_batch = None;
        }
        if let Some(item) = self.transfer_item {
            match self.store.stock_batches(company, item) {
                Ok(batches) => pick_batch(ui, "transfer-batch", &batches, &mut self.transfer_batch),
                Err(e) => self.failure(e),
            }
        }
        ui.horizontal(|ui| {
            pick_location(
                ui,
                "transfer-from",
                "From",
                &locations,
                &mut self.transfer_from,
                false,
            );
            pick_location(
                ui,
                "transfer-to",
                "To",
                &locations,
                &mut self.transfer_to,
                false,
            );
        });
        ui.horizontal(|ui| {
            ui.label("Date");
            ui.add(egui::TextEdit::singleline(&mut self.transfer_date).hint_text("YYYY-MM-DD"));
            ui.label("Quantity");
            ui.text_edit_singleline(&mut self.transfer_quantity);
            ui.label("Reason");
            ui.text_edit_singleline(&mut self.transfer_reason);
        });
        let separate = self.stock_transit_screen.separate_dispatch;
        if separate {
            ui.horizontal(|ui| {
                ui.label("Dispatch reference");
                ui.text_edit_singleline(&mut self.stock_transit_screen.reference);
            });
        }
        if ui
            .button(if separate {
                "Dispatch stock"
            } else {
                "Save transfer"
            })
            .clicked()
        {
            let result = (|| -> balance::Result<i64> {
                let item = self
                    .transfer_item
                    .ok_or_else(|| balance::Error::Validation("Select a stock item".into()))?;
                let from = self
                    .transfer_from
                    .ok_or_else(|| balance::Error::Validation("Select a source location".into()))?;
                let to = self.transfer_to.ok_or_else(|| {
                    balance::Error::Validation("Select a destination location".into())
                })?;
                let quantity = parse_quantity(&self.transfer_quantity)?;
                if separate {
                    self.store.dispatch_stock(
                        company,
                        &balance::stock_transit::StockDispatchDraft {
                            item_id: item,
                            batch_id: self.transfer_batch,
                            from_location_id: from,
                            to_location_id: to,
                            date: self.transfer_date.clone(),
                            quantity,
                            reference: self.stock_transit_screen.reference.clone(),
                            reason: self.transfer_reason.clone(),
                        },
                    )
                } else {
                    self.store.transfer_stock_batch(
                        company,
                        item,
                        self.transfer_batch,
                        from,
                        to,
                        &self.transfer_date,
                        quantity,
                        &self.transfer_reason,
                    )
                }
            })();
            match result {
                Ok(_) => {
                    self.transfer_quantity.clear();
                    self.transfer_reason.clear();
                    self.stock_transit_screen.reference.clear();
                    self.message = if separate {
                        "Stock dispatched"
                    } else {
                        "Stock transfer saved"
                    }
                    .into();
                }
                Err(e) => self.failure(e),
            }
        }
        egui::CollapsingHeader::new("Stock transfer history").show(ui, |ui| {
            match self.store.stock_transfers(company) {
                Ok(transfers) => {
                    for transfer in transfers {
                        ui.label(format!(
                            "{}: {} {} {} batch {} from {} to {} — {}",
                            transfer.date,
                            transfer.item,
                            format_quantity(i128::from(transfer.quantity)),
                            transfer.unit,
                            transfer.batch.as_deref().unwrap_or("unbatched"),
                            transfer.from,
                            transfer.to,
                            transfer.reason
                        ));
                    }
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                }
            }
        });
        egui::CollapsingHeader::new("Stock in transit").show(ui, |ui| {
            if let Err(error) = self.stock_transit_view(ui, company) {
                self.failure(error);
            }
        });
        ui.separator();
        crate::theme::section(ui, "Stock quantities");
        self.report_export_path_field(ui);
        ui.small(
            "An empty as-of date includes all saved movements, including future-dated entries.",
        );
        ui.horizontal(|ui| {
            ui.label("As of (optional)");
            ui.add(egui::TextEdit::singleline(&mut self.stock_as_of).hint_text("YYYY-MM-DD"));
        });
        pick_location(
            ui,
            "balance-location",
            "Location",
            &locations,
            &mut self.stock_balance_location,
            true,
        );
        let end = if self.stock_as_of.trim().is_empty() {
            None
        } else {
            Some(self.stock_as_of.trim())
        };
        if ui.button("Export company-wide stock as CSV").clicked() {
            match self
                .store
                .export_stock_csv(company, end, self.report_export_path.trim())
            {
                Ok(()) => self.message = "Stock report exported".into(),
                Err(e) => self.message = e.to_string(),
            }
        }
        ui.small("The company-wide CSV includes current reorder settings.");
        let balances = match self.stock_balance_location {
            Some(location) => {
                ui.checkbox(
                    &mut self.stock_balance_descendants,
                    "Include descendant locations",
                );
                if ui
                    .button("Export selected location totals as CSV")
                    .clicked()
                {
                    match self.store.export_stock_location_csv(
                        company,
                        location,
                        self.stock_balance_descendants,
                        end,
                        self.report_export_path.trim(),
                    ) {
                        Ok(()) => self.message = "Location stock report exported".into(),
                        Err(error) => self.message = error.to_string(),
                    }
                }
                self.store.location_stock_quantities(
                    company,
                    location,
                    self.stock_balance_descendants,
                    end,
                )
            }
            None => self.store.stock_balances(company, end),
        };
        match balances {
            Ok(balances) => {
                for balance in balances {
                    ui.label(format!(
                        "{}: {} {}",
                        balance.item.name,
                        format_quantity(balance.quantity),
                        balance.item.unit
                    ));
                }
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
        ui.small("See Stock valuation for ageing and cost reports.");
        crate::theme::section(ui, "Low stock across all locations");
        ui.small("Uses the as-of date above and current reorder settings, regardless of the location filter. Suggested quantities are fixed and do not account for pending orders.");
        match self.store.low_stock(company, end) {
            Ok(rows) if rows.is_empty() => {
                ui.label("No enabled reorder thresholds reached.");
            }
            Ok(rows) => {
                for row in rows {
                    ui.label(format!(
                        "{}: {} {} on hand · level {} · suggested {} {}",
                        row.item.name,
                        format_quantity(row.on_hand),
                        row.item.unit,
                        format_quantity(i128::from(row.rule.level)),
                        format_quantity(i128::from(row.rule.quantity)),
                        row.item.unit
                    ));
                }
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
        ui.separator();
        crate::theme::section(ui, "Stock activity");
        ui.small("Stock activity uses the location scope above. It shows transfers into or out of that scope.");
        pick_stock(
            ui,
            "stock-register-item",
            "Item",
            &items,
            &mut self.stock_register_item,
        );
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(
                egui::TextEdit::singleline(&mut self.stock_register_from)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            ui.label("To");
            ui.add(
                egui::TextEdit::singleline(&mut self.stock_register_to)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
        });
        if let Some(item) = self.stock_register_item {
            let from = if self.stock_register_from.trim().is_empty() {
                None
            } else {
                Some(self.stock_register_from.trim())
            };
            let to = if self.stock_register_to.trim().is_empty() {
                None
            } else {
                Some(self.stock_register_to.trim())
            };
            match self.store.stock_register_in_scope(
                company,
                item,
                self.stock_balance_location,
                self.stock_balance_descendants,
                from,
                to,
            ) {
                Ok(register) => {
                    ui.label(format!(
                        "Opening: {} {}",
                        format_quantity(register.opening_quantity),
                        register.item.unit
                    ));
                    egui::Grid::new("stock-activity")
                        .striped(true)
                        .show(ui, |ui| {
                            for h in ["Date", "Movement", "Change", "Balance"] {
                                ui.strong(h);
                            }
                            ui.end_row();
                            for movement in register.movements {
                                ui.label(movement.date);
                                ui.label(movement.description);
                                ui.label(format_quantity(i128::from(movement.quantity)));
                                ui.label(format_quantity(movement.running_quantity));
                                ui.end_row();
                            }
                        });
                    ui.label(format!(
                        "Closing: {} {}",
                        format_quantity(register.closing_quantity),
                        register.item.unit
                    ));
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                }
            }
        }
    }
    fn invoice_draft_in_use(&self) -> bool {
        self.editing_invoice.is_some() || self.copying_invoice || self.invoice_source.is_some()
    }

    fn prepare_linked_invoice(
        &mut self,
        source: InvoiceSource,
        kind: InvoiceKind,
        party: i64,
        lines: Vec<InvoiceLineForm>,
    ) {
        self.invoice_source = Some(source);
        self.invoice_kind = kind;
        self.invoice_entry_mode = InvoiceEntryMode::Invoice;
        self.invoice_party = Some(party);
        self.invoice_trade = None;
        self.invoice_date.clear();
        self.invoice_reference.clear();
        self.invoice_external_number.clear();
        self.invoice_external_date.clear();
        self.invoice_bill_reference.clear();
        self.invoice_bill_splits.clear();
        self.invoice_narration.clear();
        self.invoice_due_date.clear();
        self.invoice_credit_days.clear();
        self.invoice_tax_amount.clear();
        self.invoice_additional_taxes.clear();
        self.clear_invoice_tax_context();
        self.invoice_tax_ledger = None;
        self.invoice_adjustments.clear();
        self.invoice_lines = lines;
        self.page = Page::Invoice;
        self.message.clear();
    }

    fn load_invoice_form(&mut self, draft: InvoiceDraft, edit: Option<i64>) {
        self.invoice_source = None;
        self.editing_invoice = edit;
        self.copying_invoice = edit.is_none();
        self.invoice_kind = draft.kind;
        self.invoice_entry_mode = draft.entry_mode;
        self.invoice_date = if edit.is_some() {
            draft.date
        } else {
            String::new()
        };
        self.invoice_due_date = if edit.is_some() {
            draft.due_date.unwrap_or_default()
        } else {
            String::new()
        };
        self.invoice_credit_days = draft
            .credit_days
            .map(|days| days.to_string())
            .unwrap_or_default();
        self.invoice_reference = if edit.is_some() {
            draft.reference
        } else {
            String::new()
        };
        if edit.is_some() {
            self.invoice_external_number = draft.external_reference.number.unwrap_or_default();
            self.invoice_external_date = draft.external_reference.date.unwrap_or_default();
        } else {
            self.invoice_external_number.clear();
            self.invoice_external_date.clear();
        }
        self.invoice_bill_reference = if edit.is_some() {
            draft.bill_reference.unwrap_or_default()
        } else {
            String::new()
        };
        self.invoice_bill_splits = draft
            .bill_splits
            .into_iter()
            .map(|bill| BillSplitForm {
                reference: if edit.is_some() {
                    bill.reference
                } else {
                    String::new()
                },
                amount: format_amount(bill.amount),
                due_date: if edit.is_some() {
                    bill.due_date.unwrap_or_default()
                } else {
                    String::new()
                },
                credit_days: bill
                    .credit_days
                    .map(|days| days.to_string())
                    .unwrap_or_default(),
            })
            .collect();
        self.invoice_narration = draft.narration;
        self.invoice_party = Some(draft.party_ledger_id);
        self.invoice_trade = Some(draft.trade_ledger_id);
        self.invoice_tax_ledger = draft.tax_ledger_id;
        self.invoice_tax_amount = if draft.tax_amount == 0 {
            String::new()
        } else {
            format_amount(draft.tax_amount)
        };
        self.clear_invoice_tax_context();
        if let Some(tax) = draft.tax_calculation.filter(|_| edit.is_some()) {
            self.invoice_tax_calculation = Some(tax);
            self.invoice_tax_calculation_date = self.invoice_date.clone();
            if !tax.whole_invoice {
                self.invoice_taxable_base = format_amount(tax.taxable_amount);
            }
        }
        self.invoice_additional_taxes = draft
            .additional_taxes
            .into_iter()
            .map(|tax| InvoiceTaxForm {
                ledger: Some(tax.ledger_id),
                amount: format_amount(tax.amount),
                original: (edit.is_some() && !tax.source_rates.is_empty()).then_some(tax),
            })
            .collect();
        self.invoice_lines = draft
            .lines
            .into_iter()
            .map(|line| {
                let (discount_mode, discount_value) = match line.discount {
                    InvoiceDiscount::None => (DiscountMode::None, String::new()),
                    InvoiceDiscount::Fixed(amount) => (DiscountMode::Fixed, format_amount(amount)),
                    InvoiceDiscount::Percent(rate) => (DiscountMode::Percent, rate),
                };
                InvoiceLineForm {
                    item: Some(line.item_id),
                    stock_note_line: None,
                    location: Some(line.location_id),
                    batch: line.batch_id,
                    trade_ledger: line.trade_ledger_id,
                    extra_allocations: line
                        .extra_allocations
                        .into_iter()
                        .map(|allocation| InvoiceAllocationForm {
                            location: Some(allocation.location_id),
                            batch: allocation.batch_id,
                            quantity: format_quantity(i128::from(allocation.quantity)),
                            billed_quantity: allocation
                                .billed_quantity
                                .map(|qty| format_quantity(qty.into()))
                                .unwrap_or_default(),
                        })
                        .collect(),
                    quantity: format_quantity(i128::from(line.quantity)),
                    billed_quantity: line
                        .billed_quantity
                        .map(|qty| format_quantity(qty.into()))
                        .unwrap_or_default(),
                    price: format_amount(line.unit_price),
                    rate_basis: line.rate_basis,
                    discount_mode,
                    discount_value,
                }
            })
            .collect();
        self.invoice_adjustments = draft
            .adjustments
            .into_iter()
            .map(|adjustment| InvoiceAdjustmentForm {
                ledger: Some(adjustment.ledger_id),
                amount: format_amount(adjustment.amount)
                    .trim_start_matches('-')
                    .to_owned(),
                increase: adjustment.amount > 0,
                description: adjustment.description,
            })
            .collect();
    }
    fn invoice_bill_schedule(&mut self, ui: &mut egui::Ui) {
        ui.small("Bill amounts must add up to the document total.");
        let can_remove = self.invoice_bill_splits.len() > 2;
        let mut remove = None;
        egui::Grid::new("invoice-bill-schedule").show(ui, |ui| {
            for label in ["Bill reference", "Amount", "Due date", "Credit days"] {
                ui.strong(label);
            }
            ui.end_row();
            for (index, bill) in self.invoice_bill_splits.iter_mut().enumerate() {
                ui.add(egui::TextEdit::singleline(&mut bill.reference).desired_width(160.0));
                ui.add(egui::TextEdit::singleline(&mut bill.amount).desired_width(85.0));
                ui.add(
                    egui::TextEdit::singleline(&mut bill.due_date)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(105.0),
                );
                ui.add(egui::TextEdit::singleline(&mut bill.credit_days).desired_width(65.0));
                if ui
                    .add_enabled(can_remove, egui::Button::new("Remove"))
                    .clicked()
                {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
        if let Some(index) = remove {
            self.invoice_bill_splits.remove(index);
        }
        if ui.button("Add bill").clicked() {
            self.invoice_bill_splits.push(BillSplitForm::default());
        }
    }

    fn invoice_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(
            ui,
            match self.editing_invoice {
                Some(id) => format!("Edit invoice {id}"),
                None if self.invoice_source.is_some() => "Invoice linked stock".into(),
                None if self.copying_invoice => "New invoice from copy".into(),
                None => "Sales and purchase invoices".into(),
            },
        );
        let from_note = self.invoice_source == Some(InvoiceSource::StockNotes);
        match self.invoice_source {
            Some(InvoiceSource::Order(order)) => {
                ui.label(format!(
                    "Order {order}. Review quantities and prices before saving."
                ));
            }
            Some(InvoiceSource::StockNotes) => {
                ui.label("Reduce quantities or remove lines to bill selected stock notes in part. Review prices and tax before posting.");
            }
            None => {
                ui.label("For services, remove the item lines and add a service or charge. For a stock-only invoice, record the quantities and enter zero in the billed fields. Each line rounds to two decimal places.");
            }
        }
        let items = match self.store.stock_items(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let price_units = match self.store.invoice_price_units(company) {
            Ok(units) => units,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let mut item_batches = std::collections::HashMap::new();
        for item in &items {
            match self.store.stock_batches(company, item.id) {
                Ok(batches) => {
                    item_batches.insert(item.id, batches);
                }
                Err(e) => {
                    self.failure(e);
                    return;
                }
            }
        }
        let locations = match self.store.stock_locations(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let main_location = locations.iter().find(|l| l.name == "Main").map(|l| l.id);
        for line in &mut self.invoice_lines {
            if line.location.is_none() {
                line.location = main_location;
            }
        }
        let ledgers = match self.store.ledgers(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!from_note, |ui| {
                egui::ComboBox::from_label("Type")
                    .selected_text(self.invoice_kind.as_str())
                    .show_ui(ui, |ui| {
                        for kind in InvoiceKind::ALL {
                            ui.selectable_value(&mut self.invoice_kind, kind, kind.as_str());
                        }
                    });
            });
            ui.label("Date");
            ui.add(egui::TextEdit::singleline(&mut self.invoice_date).hint_text("YYYY-MM-DD"));
            if self.invoice_bill_splits.is_empty() {
                ui.label("Due date (optional)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.invoice_due_date).hint_text("YYYY-MM-DD"),
                );
                ui.label("or credit days");
                ui.add(
                    egui::TextEdit::singleline(&mut self.invoice_credit_days)
                        .hint_text("Days")
                        .desired_width(60.0),
                );
            }
            ui.label("Reference");
            ui.text_edit_singleline(&mut self.invoice_reference);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(if self.invoice_kind == InvoiceKind::Purchase {
                "Supplier invoice number"
            } else {
                "External invoice number"
            });
            ui.text_edit_singleline(&mut self.invoice_external_number);
            ui.label("External invoice date");
            ui.add(
                egui::TextEdit::singleline(&mut self.invoice_external_date)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Entry mode")
                .selected_text(self.invoice_entry_mode.as_str())
                .show_ui(ui, |ui| {
                    for mode in InvoiceEntryMode::ALL {
                        ui.selectable_value(&mut self.invoice_entry_mode, mode, mode.as_str());
                    }
                });
            if self.invoice_bill_splits.is_empty() {
                ui.label("Bill reference");
                ui.add(
                    egui::TextEdit::singleline(&mut self.invoice_bill_reference)
                        .hint_text("Defaults to invoice reference"),
                );
            }
            let mut split = !self.invoice_bill_splits.is_empty();
            if ui.checkbox(&mut split, "Split into bills").changed() {
                if split {
                    self.invoice_bill_splits = vec![
                        BillSplitForm {
                            reference: std::mem::take(&mut self.invoice_bill_reference),
                            due_date: std::mem::take(&mut self.invoice_due_date),
                            credit_days: std::mem::take(&mut self.invoice_credit_days),
                            ..BillSplitForm::default()
                        },
                        BillSplitForm::default(),
                    ];
                } else if let Some(first) = self.invoice_bill_splits.drain(..).next() {
                    self.invoice_bill_reference = first.reference;
                    self.invoice_due_date = first.due_date;
                    self.invoice_credit_days = first.credit_days;
                }
            }
        });
        if !self.invoice_bill_splits.is_empty() {
            self.invoice_bill_schedule(ui);
        }
        let lists = match self.store.price_lists(company) {
            Ok(lists) => lists,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        ui.horizontal(|ui| {
            pick_price_list(ui, "invoice-price-list", &lists, &mut self.price_list);
            if ui.button("Fill blank item prices").clicked() {
                let prices = (|| -> balance::Result<Vec<(usize, i64)>> {
                    let list = self
                        .price_list
                        .ok_or_else(|| balance::Error::Validation("Select a price list".into()))?;
                    let mut result = Vec::new();
                    for (index, line) in self
                        .invoice_lines
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| line.price.trim().is_empty())
                    {
                        let item = line.item.ok_or_else(|| {
                            balance::Error::Validation(
                                "Select an item on every blank-price line".into(),
                            )
                        })?;
                        let price = self
                            .store
                            .stock_price(company, list, item, &self.invoice_date)?
                            .ok_or_else(|| {
                                balance::Error::Validation(format!(
                                    "No applicable price for item line {}",
                                    index + 1
                                ))
                            })?;
                        result.push((index, price));
                    }
                    if result.is_empty() {
                        return Err(balance::Error::Validation(
                            "There are no blank item prices to fill".into(),
                        ));
                    }
                    Ok(result)
                })();
                match prices {
                    Ok(prices) => {
                        for (index, price) in prices {
                            self.invoice_lines[index].price = format_amount(price);
                            self.invoice_lines[index].rate_basis = None;
                        }
                        self.message = "Blank item prices filled for the invoice date".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        ui.small("Price lookup uses the invoice date and only fills blank prices. A saved invoice keeps its entered price, not the price-list link.");
        ui.add_enabled_ui(!from_note, |ui| {
            pick_account(
                ui,
                "invoice-party",
                "Party",
                &ledgers,
                &mut self.invoice_party,
                false,
            );
        });
        if let Some(party) = self.invoice_party {
            if ledgers.iter().any(|ledger| {
                ledger.id == party
                    && matches!(ledger.category, Category::Asset | Category::Liability)
            }) {
                match self.store.ledger_terms(company, party) {
                    Ok(terms) => {
                        if let Some(limit) = terms.credit_limit {
                            ui.small(format!(
                                "Party credit limit: {} (informational; not enforced)",
                                format_amount(limit)
                            ));
                        }
                        if let Some(days) = terms.credit_days {
                            if ui
                                .add_enabled(
                                    self.invoice_due_date.trim().is_empty()
                                        && self.invoice_credit_days.trim().is_empty()
                                        && self.invoice_bill_splits.is_empty(),
                                    egui::Button::new(format!(
                                        "Apply default credit period: {days} days"
                                    )),
                                )
                                .clicked()
                            {
                                self.invoice_credit_days = days.to_string();
                            }
                        }
                    }
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                    }
                }
            }
        }
        pick_account(
            ui,
            "invoice-trade",
            "Default sales / purchase ledger (required)",
            &ledgers,
            &mut self.invoice_trade,
            false,
        );
        self.invoice_tax_profile_form(ui, company);
        ui.add_enabled_ui(self.invoice_tax_calculation.is_none(), |ui| {
            pick_account(
                ui,
                "invoice-tax",
                "Tax ledger (optional)",
                &ledgers,
                &mut self.invoice_tax_ledger,
                true,
            );
            ui.horizontal(|ui| {
                ui.label("Tax amount");
                ui.add(egui::TextEdit::singleline(&mut self.invoice_tax_amount).hint_text("0.00"));
            });
            let mut remove_tax = None;
            for (index, tax) in self.invoice_additional_taxes.iter_mut().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    pick_account(
                        ui,
                        ("invoice-additional-tax", index),
                        "Tax ledger",
                        &ledgers,
                        &mut tax.ledger,
                        false,
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut tax.amount)
                            .hint_text("Tax amount")
                            .desired_width(100.0),
                    );
                    if let Ok(amount) = parse_amount(&tax.amount) {
                        let rates = tax.source_rates(amount);
                        if !rates.is_empty() {
                            ui.small(format!("Source rates {}", format_source_tax_rates(rates)));
                        }
                    }
                    if ui.button("Remove tax").clicked() {
                        remove_tax = Some(index);
                    }
                });
            }
            if let Some(index) = remove_tax {
                self.invoice_additional_taxes.remove(index);
            }
            if ui.button("Add tax component").clicked() {
                self.invoice_additional_taxes
                    .push(InvoiceTaxForm::default());
            }
        });
        ui.small("Review the tax amounts before saving. Percentage discounts apply to the rounded gross line amount.");
        let mut remove = None;
        for (index, line) in self.invoice_lines.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                if let Some((_, reference)) = &line.stock_note_line {
                    ui.label(reference);
                } else {
                    ui.label(format!("{}.", index + 1));
                }
                ui.add_enabled_ui(!from_note, |ui| {
                    let previous_item = line.item;
                    pick_stock(ui, ("invoice-item", index), "Item", &items, &mut line.item);
                    if previous_item != line.item {
                        if line.rate_basis.take().is_some() {
                            line.price.clear();
                        }
                        line.batch = None;
                        for allocation in &mut line.extra_allocations {
                            allocation.batch = None;
                        }
                    }
                    if let Some(batches) = line.item.and_then(|item| item_batches.get(&item)) {
                        pick_batch(ui, ("invoice-batch", index), batches, &mut line.batch);
                    }
                    pick_location(
                        ui,
                        ("invoice-location", index),
                        "Location",
                        &locations,
                        &mut line.location,
                        false,
                    );
                });
                let trade = ledgers
                    .iter()
                    .find(|ledger| Some(ledger.id) == line.trade_ledger)
                    .map(|ledger| ledger.name.as_str())
                    .unwrap_or("Default ledger");
                egui::ComboBox::from_id_salt(("invoice-item-ledger", index))
                    .selected_text(trade)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut line.trade_ledger, None, "Default ledger");
                        for ledger in &ledgers {
                            ui.selectable_value(
                                &mut line.trade_ledger,
                                Some(ledger.id),
                                &ledger.name,
                            );
                        }
                    });
                let stock_item = items.iter().find(|item| Some(item.id) == line.item);
                let quantity_hint = stock_item.map_or_else(
                    || "Quantity".into(),
                    |item| format!("Stock qty ({})", item.unit),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut line.quantity)
                        .hint_text(quantity_hint)
                        .desired_width(90.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut line.billed_quantity)
                        .hint_text("Billed qty")
                        .desired_width(78.0),
                )
                .on_hover_text("Leave blank to bill the stock quantity. Enter 0 for free stock.");
                ui.add(
                    egui::TextEdit::singleline(&mut line.price)
                        .hint_text(line.rate_basis.as_ref().map_or_else(
                            || "Unit price".into(),
                            |basis| format!("Price per {}", basis.unit),
                        ))
                        .desired_width(90.0),
                );
                if let Some(item) = stock_item {
                    let choices = price_units.get(&item.unit_id);
                    if choices.is_some() || line.rate_basis.is_some() {
                        let previous = line.rate_basis.clone();
                        let unit = previous
                            .as_ref()
                            .map_or(item.unit.as_str(), |basis| basis.unit.as_str());
                        egui::ComboBox::from_id_salt(("invoice-price-unit", index))
                            .selected_text(format!("per {unit}"))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut line.rate_basis,
                                    None,
                                    format!("per {}", item.unit),
                                );
                                for basis in choices.into_iter().flatten() {
                                    ui.selectable_value(
                                        &mut line.rate_basis,
                                        Some(basis.clone()),
                                        format!("per {}", basis.unit),
                                    );
                                }
                            });
                        if previous != line.rate_basis {
                            line.price.clear();
                        }
                    }
                }
                egui::ComboBox::from_id_salt(("invoice-discount", index))
                    .selected_text(match line.discount_mode {
                        DiscountMode::None => "No discount",
                        DiscountMode::Fixed => "Fixed discount",
                        DiscountMode::Percent => "Discount %",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut line.discount_mode,
                            DiscountMode::None,
                            "No discount",
                        );
                        ui.selectable_value(
                            &mut line.discount_mode,
                            DiscountMode::Fixed,
                            "Fixed discount",
                        );
                        ui.selectable_value(
                            &mut line.discount_mode,
                            DiscountMode::Percent,
                            "Discount %",
                        );
                    });
                if line.discount_mode != DiscountMode::None {
                    ui.add(
                        egui::TextEdit::singleline(&mut line.discount_value)
                            .hint_text("Discount")
                            .desired_width(80.0),
                    );
                }
                if let (Ok(qty), Ok(price), Ok(discount)) = (
                    parse_optional_quantity(&line.billed_quantity).and_then(|qty| match qty {
                        Some(qty) => Ok(qty),
                        None => parse_quantity(&line.quantity),
                    }),
                    line.price_amount(),
                    line.discount(),
                ) {
                    if let Ok((amount, applied)) = discounted_line_amount_with_basis(
                        qty,
                        price,
                        &discount,
                        line.rate_basis.as_ref(),
                    ) {
                        ui.label(format!(
                            "Discount {} · Amount {}",
                            format_amount(applied),
                            format_amount(amount)
                        ));
                    }
                }
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
            let mut remove_allocation = None;
            for (allocation_index, allocation) in line.extra_allocations.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label("Additional allocation");
                    pick_location(
                        ui,
                        ("invoice-extra-location", index, allocation_index),
                        "Location",
                        &locations,
                        &mut allocation.location,
                        false,
                    );
                    if let Some(batches) = line.item.and_then(|item| item_batches.get(&item)) {
                        pick_batch(
                            ui,
                            ("invoice-extra-batch", index, allocation_index),
                            batches,
                            &mut allocation.batch,
                        );
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut allocation.quantity)
                            .hint_text("Quantity")
                            .desired_width(90.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut allocation.billed_quantity)
                            .hint_text("Billed qty")
                            .desired_width(78.0),
                    );
                    if ui.button("Remove allocation").clicked() {
                        remove_allocation = Some(allocation_index);
                    }
                });
            }
            if let Some(allocation_index) = remove_allocation {
                line.extra_allocations.remove(allocation_index);
            }
            if !line.extra_allocations.is_empty() {
                ui.small("The first location receives the remaining stock and billed quantities.");
            }
            if !from_note && ui.button("Add location or batch allocation").clicked() {
                line.extra_allocations.push(InvoiceAllocationForm {
                    location: main_location,
                    batch: None,
                    quantity: String::new(),
                    billed_quantity: String::new(),
                });
            }
        }
        if let Some(index) = remove {
            self.invoice_lines.remove(index);
        }
        if !from_note && ui.button("Add item line").clicked() {
            self.invoice_lines.push(InvoiceLineForm::new());
        }
        crate::theme::section(ui, "Services, charges and deductions");
        ui.small("Enter each amount and description manually. For an invoice without stock items, include a positive line in an income account for a sale, or an expense or asset account for a purchase. These lines do not calculate or classify tax.");
        let mut remove_adjustment = None;
        for (index, adjustment) in self.invoice_adjustments.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                pick_account(
                    ui,
                    ("invoice-adjustment", index),
                    "Ledger",
                    &ledgers,
                    &mut adjustment.ledger,
                    false,
                );
                egui::ComboBox::from_id_salt(("adjustment-direction", index))
                    .selected_text(if adjustment.increase {
                        "Increase total"
                    } else {
                        "Decrease total"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut adjustment.increase, true, "Increase total");
                        ui.selectable_value(&mut adjustment.increase, false, "Decrease total");
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut adjustment.amount)
                        .hint_text("Amount")
                        .desired_width(90.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut adjustment.description)
                        .hint_text("Description")
                        .desired_width(130.0),
                );
                if ui.button("Remove").clicked() {
                    remove_adjustment = Some(index);
                }
            });
        }
        if let Some(index) = remove_adjustment {
            self.invoice_adjustments.remove(index);
        }
        if ui.button("Add service, charge or deduction").clicked() {
            self.invoice_adjustments.push(InvoiceAdjustmentForm {
                ledger: None,
                amount: String::new(),
                increase: true,
                description: String::new(),
            });
        }
        ui.horizontal(|ui| {
            ui.label("Narration");
            ui.text_edit_singleline(&mut self.invoice_narration);
        });
        if self.invoice_draft_in_use() && ui.button("Cancel invoice draft").clicked() {
            self.editing_invoice = None;
            self.copying_invoice = false;
            self.invoice_source = None;
            self.invoice_date.clear();
            self.invoice_party = None;
            self.invoice_trade = None;
            self.invoice_reference.clear();
            self.invoice_external_number.clear();
            self.invoice_external_date.clear();
            self.invoice_bill_reference.clear();
            self.invoice_bill_splits.clear();
            self.invoice_narration.clear();
            self.invoice_due_date.clear();
            self.invoice_credit_days.clear();
            self.invoice_lines = vec![InvoiceLineForm::new()];
            self.invoice_adjustments.clear();
            self.invoice_tax_amount.clear();
            self.invoice_additional_taxes.clear();
            self.clear_invoice_tax_context();
            self.invoice_tax_ledger = None;
        }
        if theme::primary_button(ui, "Save invoice").clicked() {
            let result = (|| -> balance::Result<i64> {
                let mut lines = Vec::new();
                for line in &self.invoice_lines {
                    let item_id = line.item.ok_or_else(|| {
                        balance::Error::Validation("Select an item for every line".into())
                    })?;
                    lines.push(InvoiceLine {
                        item_id,
                        location_id: line.location.ok_or_else(|| {
                            balance::Error::Validation("Select a location for every line".into())
                        })?,
                        batch_id: line.batch,
                        trade_ledger_id: line.trade_ledger,
                        discount: line.discount()?,
                        extra_allocations: line
                            .extra_allocations
                            .iter()
                            .map(|allocation| {
                                Ok(InvoiceAllocation {
                                    location_id: allocation.location.ok_or_else(|| {
                                        balance::Error::Validation(
                                            "Select a location for every allocation".into(),
                                        )
                                    })?,
                                    batch_id: allocation.batch,
                                    quantity: parse_quantity(&allocation.quantity)?,
                                    billed_quantity: parse_optional_quantity(
                                        &allocation.billed_quantity,
                                    )?,
                                })
                            })
                            .collect::<balance::Result<Vec<_>>>()?,
                        quantity: parse_quantity(&line.quantity)?,
                        billed_quantity: parse_optional_quantity(&line.billed_quantity)?,
                        unit_price: line.price_amount()?,
                        rate_basis: line.rate_basis.clone(),
                    });
                }
                let tax_amount = if self.invoice_tax_amount.trim().is_empty() {
                    0
                } else {
                    parse_amount(&self.invoice_tax_amount)?
                };
                let additional_taxes = self
                    .invoice_additional_taxes
                    .iter()
                    .map(|tax| {
                        let amount = parse_amount(&tax.amount)?;
                        Ok(InvoiceTaxLine {
                            ledger_id: tax.ledger.ok_or_else(|| {
                                balance::Error::Validation(
                                    "Choose a ledger for every tax component".into(),
                                )
                            })?,
                            amount,
                            source_rates: tax.source_rates(amount).to_vec(),
                        })
                    })
                    .collect::<balance::Result<Vec<_>>>()?;
                let mut adjustments = Vec::new();
                for line in &self.invoice_adjustments {
                    let amount = parse_amount(&line.amount)?;
                    adjustments.push(InvoiceAdjustment {
                        ledger_id: line.ledger.ok_or_else(|| {
                            balance::Error::Validation(
                                "Select a ledger for every adjustment".into(),
                            )
                        })?,
                        amount: if line.increase { amount } else { -amount },
                        description: line.description.clone(),
                    });
                }
                if self.invoice_tax_calculation.is_some()
                    && self.invoice_tax_calculation_date != self.invoice_date.trim()
                {
                    return Err(balance::Error::Validation(
                        "The invoice date changed. Recalculate taxes or keep manual amounts."
                            .into(),
                    ));
                }
                let draft = InvoiceDraft {
                    kind: self.invoice_kind,
                    entry_mode: self.invoice_entry_mode,
                    date: self.invoice_date.clone(),
                    due_date: (!self.invoice_due_date.trim().is_empty())
                        .then(|| self.invoice_due_date.trim().to_owned()),
                    credit_days: if self.invoice_credit_days.trim().is_empty() {
                        None
                    } else {
                        Some(
                            self.invoice_credit_days
                                .trim()
                                .parse::<u32>()
                                .map_err(|_| {
                                    balance::Error::Validation(
                                        "Credit days must be a non-negative whole number".into(),
                                    )
                                })?,
                        )
                    },
                    reference: self.invoice_reference.clone(),
                    external_reference: InvoiceExternalReference {
                        number: (!self.invoice_external_number.trim().is_empty())
                            .then(|| self.invoice_external_number.trim().to_owned()),
                        date: (!self.invoice_external_date.trim().is_empty())
                            .then(|| self.invoice_external_date.trim().to_owned()),
                    },
                    bill_reference: (!self.invoice_bill_reference.trim().is_empty())
                        .then(|| self.invoice_bill_reference.trim().to_owned()),
                    bill_splits: self
                        .invoice_bill_splits
                        .iter()
                        .map(BillSplitForm::draft)
                        .collect::<balance::Result<Vec<_>>>()?,
                    narration: self.invoice_narration.clone(),
                    party_ledger_id: self.invoice_party.ok_or_else(|| {
                        balance::Error::Validation("Select a party ledger".into())
                    })?,
                    trade_ledger_id: self.invoice_trade.ok_or_else(|| {
                        balance::Error::Validation("Select a sales or purchase ledger".into())
                    })?,
                    tax_ledger_id: self.invoice_tax_ledger,
                    tax_amount,
                    additional_taxes,
                    tax_calculation: self.invoice_tax_calculation,
                    lines,
                    adjustments,
                };
                if let Some(id) = self.editing_invoice {
                    self.store.update_invoice(company, id, &draft)?;
                    Ok(id)
                } else {
                    match self.invoice_source {
                        Some(InvoiceSource::Order(order)) => {
                            self.store.invoice_order(company, order, &draft)
                        }
                        Some(InvoiceSource::StockNotes) => {
                            let sources = self
                                .invoice_lines
                                .iter()
                                .map(|line| {
                                    line.stock_note_line.as_ref().map(|(id, _)| *id).ok_or_else(
                                        || {
                                            balance::Error::Validation(
                                                "Select a stock note source for each line".into(),
                                            )
                                        },
                                    )
                                })
                                .collect::<balance::Result<Vec<_>>>()?;
                            self.store.invoice_stock_notes(company, &sources, &draft)
                        }
                        None => self.store.create_invoice(company, &draft),
                    }
                }
            })();
            match result {
                Ok(id) => {
                    self.message = format!("Invoice {id} saved");
                    self.invoice_reference.clear();
                    self.invoice_external_number.clear();
                    self.invoice_external_date.clear();
                    self.invoice_bill_reference.clear();
                    self.invoice_bill_splits.clear();
                    self.invoice_narration.clear();
                    self.invoice_due_date.clear();
                    self.invoice_credit_days.clear();
                    self.invoice_lines = vec![InvoiceLineForm::new()];
                    self.invoice_adjustments.clear();
                    self.invoice_tax_amount.clear();
                    self.invoice_additional_taxes.clear();
                    self.clear_invoice_tax_context();
                    self.invoice_tax_ledger = None;
                    self.editing_invoice = None;
                    self.copying_invoice = false;
                    self.invoice_source = None;
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        crate::theme::section(ui, "Invoices");
        ui.small("Invoice balances include all saved settlements and reversals. Use Outstanding for a specific date.");
        if let Some(voucher) = self.reversal_target {
            crate::theme::section(ui, format!("Reverse settlement voucher {voucher}"));
            ui.label("This reverses the cash and all discounts, bill allocations, advances and refunds on the voucher. Release any active advance applications first.");
            ui.horizontal(|ui| {
                ui.label("Date");
                ui.add(egui::TextEdit::singleline(&mut self.reversal_date).hint_text("YYYY-MM-DD"));
                ui.label("Reference");
                ui.text_edit_singleline(&mut self.reversal_reference);
                ui.label("Reason");
                ui.text_edit_singleline(&mut self.reversal_reason);
            });
            ui.horizontal(|ui| {
                if ui.button("Save reversal").clicked() {
                    match self.store.reverse_settlement(
                        company,
                        voucher,
                        &self.reversal_date,
                        &self.reversal_reference,
                        &self.reversal_reason,
                    ) {
                        Ok(id) => {
                            self.reversal_target = None;
                            self.reversal_date.clear();
                            self.reversal_reference.clear();
                            self.reversal_reason.clear();
                            self.message = format!("Reversal voucher {id} saved");
                        }
                        Err(e) => self.failure(e),
                    }
                }
                if ui.button("Cancel reversal").clicked() {
                    self.reversal_target = None;
                }
            });
        }
        match self.store.invoices(company) {
            Ok(invoices) => {
                crate::theme::section(ui, "Record a receipt or payment");
                ui.small("Choose outstanding bills of the same party and type.");
                let bill_choices = outstanding_bill_choices(&invoices);
                pick_settlement_bill(
                    ui,
                    "settlement-bill",
                    &bill_choices,
                    &mut self.settlement_invoice,
                );
                pick_account(
                    ui,
                    "settlement-bank",
                    "Cash / bank ledger",
                    &ledgers,
                    &mut self.settlement_bank,
                    false,
                );
                ui.horizontal(|ui| {
                    ui.label("Date");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settlement_date)
                            .hint_text("YYYY-MM-DD"),
                    );
                    ui.label("Reference");
                    ui.text_edit_singleline(&mut self.settlement_reference);
                    ui.label("Amount");
                    ui.text_edit_singleline(&mut self.settlement_amount);
                });
                let mut remove_allocation = None;
                for (index, line) in self.settlement_extra.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        pick_settlement_bill(
                            ui,
                            ("settlement-extra", index),
                            &bill_choices,
                            &mut line.target,
                        );
                        ui.label("Amount");
                        ui.text_edit_singleline(&mut line.amount);
                        if ui.button("Remove").clicked() {
                            remove_allocation = Some(index);
                        }
                    });
                }
                if let Some(index) = remove_allocation {
                    self.settlement_extra.remove(index);
                }
                if ui.button("Add bill allocation").clicked() {
                    self.settlement_extra.push(SettlementLineForm {
                        target: None,
                        amount: String::new(),
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Additional amount to keep on account");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settlement_on_account)
                            .hint_text("Optional"),
                    );
                });
                advance_ui::advance_part_fields(ui, &mut self.settlement_advances);
                if !self.settlement_discounts.is_empty() {
                    let accounts: Vec<_> = ledgers
                        .iter()
                        .filter(|ledger| {
                            matches!(ledger.category, Category::Income | Category::Expense)
                        })
                        .cloned()
                        .collect();
                    let mut remove = None;
                    for (index, line) in self.settlement_discounts.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            pick_account(
                                ui,
                                ("settlement-discount", index),
                                "Discount ledger",
                                &accounts,
                                &mut line.ledger,
                                false,
                            );
                            ui.label("Amount");
                            ui.text_edit_singleline(&mut line.amount);
                            if ui.button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    }
                    if let Some(index) = remove {
                        self.settlement_discounts.remove(index);
                    }
                    ui.small("Enter the amount applied to each bill before discounts.");
                }
                if ui.button("Add settlement discount").clicked() {
                    self.settlement_discounts
                        .push(SettlementDiscountForm::default());
                }
                if ui.button("Save receipt / payment").clicked() {
                    let result = (|| -> balance::Result<i64> {
                        let mut allocations = vec![BillSettlementAllocation {
                            target: self.settlement_invoice.ok_or_else(|| {
                                balance::Error::Validation("Select a bill".into())
                            })?,
                            amount: parse_amount(&self.settlement_amount)?,
                        }];
                        for line in &self.settlement_extra {
                            allocations.push(BillSettlementAllocation {
                                target: line.target.ok_or_else(|| {
                                    balance::Error::Validation(
                                        "Select a bill for every allocation".into(),
                                    )
                                })?,
                                amount: parse_amount(&line.amount)?,
                            });
                        }
                        let on_account = if self.settlement_on_account.trim().is_empty() {
                            0
                        } else {
                            parse_amount(&self.settlement_on_account)?
                        };
                        if on_account < 0 {
                            return Err(balance::Error::Validation(
                                "Amount kept on account cannot be negative".into(),
                            ));
                        }
                        let discounts = self
                            .settlement_discounts
                            .iter()
                            .map(|line| {
                                Ok(SettlementDiscount {
                                    ledger_id: line.ledger.ok_or_else(|| {
                                        balance::Error::Validation(
                                            "Select a ledger for every discount".into(),
                                        )
                                    })?,
                                    amount: parse_amount(&line.amount)?,
                                })
                            })
                            .collect::<balance::Result<Vec<_>>>()?;
                        let bank = self.settlement_bank.ok_or_else(|| {
                            balance::Error::Validation("Select a cash or bank ledger".into())
                        })?;
                        let (voucher, _) =
                            if self.settlement_advances.is_empty() && discounts.is_empty() {
                                self.store.settle_bills_with_advance(
                                    company,
                                    bank,
                                    &self.settlement_date,
                                    &self.settlement_reference,
                                    &allocations,
                                    on_account,
                                )?
                            } else {
                                let advances = advance_ui::advance_part_drafts(
                                    &self.settlement_advances,
                                    on_account,
                                )?;
                                self.store.settle_bills_with_discounts(
                                    company,
                                    bank,
                                    &self.settlement_date,
                                    &self.settlement_reference,
                                    &allocations,
                                    &advances,
                                    &discounts,
                                )?
                            };
                        Ok(voucher)
                    })();
                    match result {
                        Ok(id) => {
                            self.message = format!("Receipt / payment voucher {id} saved");
                            self.settlement_invoice = None;
                            self.settlement_reference.clear();
                            self.settlement_amount.clear();
                            self.settlement_on_account.clear();
                            self.settlement_advances.clear();
                            self.settlement_discounts.clear();
                            self.settlement_extra.clear();
                        }
                        Err(e) => self.failure(e),
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("New document HTML file");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.invoice_document_path)
                            .hint_text("Absolute path ending in .html")
                            .desired_width(350.0),
                    );
                });
                ui.small("Export a saved invoice or note for printing in a browser. Use a new file path. The document uses current company and party details.");
                ui.small("Editing or copying an invoice replaces the current invoice form. A copy retains prices, tax and allocations for review; enter a new date and reference before saving.");
                for invoice in invoices {
                    ui.strong(format!(
                        "{}  {}  {}  {}  Total {}  Settled {}  Advances {}  Notes {}  Outstanding {}",
                        invoice.date,
                        invoice.document_type(),
                        reference_with_bill(&invoice.reference, &invoice.bill_reference),
                        invoice.party,
                        money(invoice.total),
                        money(invoice.settled),
                        money(invoice.advance_applied),
                        money(invoice.credited),
                        money(invoice.outstanding)
                    ));
                    if let Some(bill) = &invoice.opening_bill {
                        ui.label(format!("Original bill date {}", bill.original_date));
                    }
                    if let Some(reference) = external_invoice_label(&invoice.external_reference) {
                        ui.label(format!("External invoice {reference}"));
                    }
                    for bill in &invoice.bill_splits {
                        ui.label(format!(
                            "Bill {} · {} · Due {} · Settled {} · Advances {} · Credits {} · Outstanding {}",
                            bill.reference,
                            format_amount(bill.amount),
                            bill.due_date.as_deref().unwrap_or("—"),
                            money(bill.settled),
                            money(bill.advance_applied),
                            money(bill.credited),
                            money(bill.outstanding())
                        ));
                    }
                    if invoice.opening_bill.is_none()
                        && ui.button("Export printable invoice").clicked()
                    {
                        match self.store.export_invoice_html(
                            company,
                            invoice.id,
                            self.invoice_document_path.trim(),
                        ) {
                            Ok(()) => self.message = "Invoice document created".into(),
                            Err(e) => self.failure(e),
                        }
                    }
                    if ui
                        .add_enabled(
                            !invoice.has_settlement_history
                                && !invoice.has_advance_history
                                && !invoice.has_credit_history
                                && invoice.reversal.is_none()
                                && invoice.order_id.is_none()
                                && invoice.stock_note_id.is_none()
                                && invoice.opening_bill.is_none()
                                && !self.invoice_draft_in_use(),
                            egui::Button::new("Edit invoice"),
                        )
                        .clicked()
                    {
                        match self.store.invoice_draft(company, invoice.id) {
                            Ok(draft) => {
                                self.load_invoice_form(draft, Some(invoice.id));
                                self.message = "Invoice loaded for editing".into();
                            }
                            Err(e) => self.failure(e),
                        }
                    }
                    if ui
                        .add_enabled(
                            !self.invoice_draft_in_use() && invoice.opening_bill.is_none(),
                            egui::Button::new("Prepare copy (replace draft)"),
                        )
                        .clicked()
                    {
                        match self.store.invoice_draft(company, invoice.id) {
                            Ok(draft) => {
                                self.load_invoice_form(draft, None);
                                self.message = "Copy prepared; review details and enter a new date and reference".into();
                            }
                            Err(e) => self.failure(e),
                        }
                    }
                    self.invoice_reversal_form(ui, company, &invoice);
                    self.invoice_credit_panel(ui, company, &invoice);
                    for &note in &invoice.stock_note_ids {
                        if ui.link(format!("View stock note {note}")).clicked() {
                            self.open_stock_note(note);
                        }
                    }
                    for &order in &invoice.order_ids {
                        if ui.link(format!("View linked order {order}")).clicked() {
                            self.order_screen.select(order);
                            self.page = Page::Orders;
                            self.message.clear();
                        }
                    }
                    if invoice.has_settlement_history || invoice.has_advance_history {
                        ui.small("Invoice details are fixed once a settlement or advance has been applied.");
                    }
                    if let Some(due_date) = &invoice.due_date {
                        ui.small(format!("Due: {due_date}"));
                    }
                    if let Some(days) = invoice.credit_days {
                        let source = if invoice.opening_bill.is_some() {
                            "original bill date"
                        } else {
                            "invoice date"
                        };
                        ui.small(format!("Credit period: {days} days from {source}"));
                    }
                    for line in invoice.lines {
                        ui.label(format!(
                            "  {} — ledger {}: {} · {} · {} · net {}",
                            line.item,
                            line.trade_ledger,
                            line.quantity_label(),
                            line.price_label(),
                            discount_description(&line.discount, line.discount_amount),
                            format_amount(line.amount)
                        ));
                        for allocation in line.allocations {
                            ui.label(format!(
                                "    {} at {}, batch {}",
                                allocation.quantity_label(&line.unit),
                                allocation.location,
                                allocation.batch.as_deref().unwrap_or("unbatched")
                            ));
                        }
                    }
                    if let Some(calculated) = &invoice.tax_calculation {
                        ui.small(format!(
                            "Tax profile {} v{} | Taxable amount {}",
                            calculated.profile,
                            calculated.calculation.version_id,
                            format_amount(calculated.calculation.taxable_amount)
                        ));
                    }
                    for tax in &invoice.taxes {
                        ui.label(invoice_tax_detail_label(tax));
                    }
                    for adjustment in invoice.adjustments {
                        ui.label(format!(
                            "  {}: {} ({})",
                            adjustment.ledger,
                            format_amount(adjustment.amount),
                            adjustment.description
                        ));
                    }
                    match self.store.invoice_settlements(company, invoice.id) {
                        Ok(rows) => {
                            for payment in rows {
                                ui.horizontal(|ui| {
                                    ui.label(format!(
                                        "  {} — {} — {} — {} (voucher #{})",
                                        payment.date,
                                        payment.reference,
                                        payment.bank_ledger,
                                        format_amount(payment.amount),
                                        payment.voucher_id
                                    ));
                                    if let (Some(id), Some(date)) = (
                                        payment.reversal_voucher_id,
                                        payment.reversal_date.as_deref(),
                                    ) {
                                        ui.label(format!(
                                            "Reversed on {date} by voucher #{id}: {}",
                                            payment.reversal_reason.as_deref().unwrap_or("")
                                        ));
                                    } else if ui
                                        .add_enabled(
                                            self.reversal_target.is_none(),
                                            egui::Button::new("Reverse whole payment"),
                                        )
                                        .clicked()
                                    {
                                        self.reversal_target = Some(payment.voucher_id);
                                    }
                                });
                                if payment.voucher_discount > 0 {
                                    ui.small(format!(
                                        "Voucher cash {} · Discounts {}",
                                        money(i128::from(payment.voucher_cash)),
                                        money(i128::from(payment.voucher_discount))
                                    ));
                                }
                                for (reference, amount) in payment.bill_allocations {
                                    ui.small(format!(
                                        "Bill {reference} · {}",
                                        format_amount(amount)
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                        }
                    }
                    ui.small(format!("Accounting voucher #{}", invoice.voucher_id));
                    egui::CollapsingHeader::new("Previous invoice versions")
                        .id_salt(("invoice-revisions", invoice.id))
                        .show(ui, |ui| {
                            match self.store.invoice_revisions(company, invoice.id) {
                                Ok(revisions) if revisions.is_empty() => {
                                    ui.label("No previous versions.");
                                }
                                Ok(revisions) => {
                                    for revision in revisions {
                                        ui.label(format!(
                                            "Replaced at {} UTC • {} • {} • {} • {}",
                                            revision.changed_at,
                                            revision.date,
                                            revision.entry_mode.label(revision.kind),
                                            reference_with_bill(
                                                &revision.reference,
                                                &revision.bill_reference
                                            ),
                                            revision.party
                                        ));
                                        if let Some(reference) =
                                            external_invoice_label(&revision.external_reference)
                                        {
                                            ui.label(format!("External invoice {reference}"));
                                        }
                                        ui.label(format!("Trade ledger: {}", revision.trade));
                                        if let Some(calculated) = &revision.tax_calculation {
                                            ui.small(format!(
                                                "Tax profile {} v{} | Taxable amount {}",
                                                calculated.profile,
                                                calculated.calculation.version_id,
                                                format_amount(
                                                    calculated.calculation.taxable_amount
                                                )
                                            ));
                                        }
                                        for tax in &revision.taxes {
                                            ui.label(invoice_tax_detail_label(tax));
                                        }
                                        if let Some(due_date) = &revision.due_date {
                                            ui.label(format!("Due: {due_date}"));
                                        }
                                        for bill in &revision.bill_splits {
                                            ui.label(format!(
                                                "Bill {} · {} · Due {}",
                                                bill.reference,
                                                format_amount(bill.amount),
                                                bill.due_date.as_deref().unwrap_or("—")
                                            ));
                                        }
                                        if let Some(days) = revision.credit_days {
                                            ui.label(format!(
                                                "Credit period: {days} days from invoice date"
                                            ));
                                        }
                                        if !revision.narration.is_empty() {
                                            ui.label(revision.narration);
                                        }
                                        for line in revision.lines {
                                            ui.label(format!(
                                                "  {} — ledger {}: {} · {} · {} · net {}",
                                                line.item,
                                                line.trade_ledger,
                                                line.quantity_label(),
                                                line.price_label(),
                                                discount_description(
                                                    &line.discount,
                                                    line.discount_amount
                                                ),
                                                format_amount(line.amount)
                                            ));
                                            for allocation in line.allocations {
                                                ui.label(format!(
                                                    "    {} at {}, batch {}",
                                                    allocation.quantity_label(&line.unit),
                                                    allocation.location,
                                                    allocation
                                                        .batch
                                                        .as_deref()
                                                        .unwrap_or("unbatched")
                                                ));
                                            }
                                        }
                                        for adjustment in revision.adjustments {
                                            ui.label(format!(
                                                "  {}: {} ({})",
                                                adjustment.ledger,
                                                format_amount(adjustment.amount),
                                                adjustment.description
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                                }
                            }
                        });
                }
            }
            Err(e) => self.failure(e),
        }
    }
    fn invoice_reversal_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        invoice: &balance::inventory::InvoiceSummary,
    ) {
        if let Some(reversal) = &invoice.reversal {
            if self
                .invoice_reversal
                .as_ref()
                .is_some_and(|form| form.invoice_id == invoice.id)
            {
                self.invoice_reversal = None;
            }
            ui.label(format!(
                "Reversed {} | {} (voucher {}) | {}",
                reversal.date, reversal.reference, reversal.voucher_id, reversal.reason
            ));
            return;
        }
        let note = match (invoice.opening_bill.is_some(), invoice.kind) {
            (true, _) => "clearing entry",
            (false, InvoiceKind::Sale) => "credit note",
            (false, InvoiceKind::Purchase) => "debit note",
        };
        let can_reverse = invoice.settled == 0
            && invoice.advance_applied == 0
            && invoice.credit_total == 0
            && !invoice.has_active_stock_returns
            && invoice.credited == 0
            && self.editing_invoice != Some(invoice.id);
        if self.invoice_reversal.is_none()
            && ui
                .add_enabled(
                    can_reverse,
                    egui::Button::new(format!("Reverse with a {note}")),
                )
                .on_disabled_hover_text(
                    "Finish editing, reverse active settlements and notes, and release applied advances before reversing the invoice.",
                )
                .clicked()
        {
            self.invoice_reversal = Some(InvoiceReversalForm {
                invoice_id: invoice.id,
                date: String::new(),
                reference: String::new(),
                reason: String::new(),
            });
        }
        let Some(form) = &mut self.invoice_reversal else {
            return;
        };
        if form.invoice_id != invoice.id {
            return;
        }
        if invoice.opening_bill.is_some() {
            ui.small("This offsets the opening bill against the opening difference account.");
        } else if invoice.stock_note_id.is_some() {
            ui.small("This posts a financial reversal. Reverse the linked stock note separately if the goods movement also needs correction.");
        } else {
            ui.small("This reverses the whole invoice and its stock quantities. The original invoice remains in the history.");
        }
        egui::Grid::new(("reverse-invoice", invoice.id)).show(ui, |ui| {
            for (label, value) in [
                ("Note date (YYYY-MM-DD)", &mut form.date),
                ("Note reference", &mut form.reference),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let mut dismiss = false;
        let mut post = false;
        ui.horizontal(|ui| {
            post = ui
                .add_enabled(can_reverse, egui::Button::new(format!("Post {note}")))
                .clicked();
            dismiss = ui.button("Cancel").clicked();
        });
        if post {
            match self.store.reverse_invoice(
                company,
                invoice.id,
                &form.date,
                &form.reference,
                &form.reason,
            ) {
                Ok(voucher) => {
                    self.message = format!("Invoice reversed with voucher {voucher}");
                    dismiss = true;
                }
                Err(error) => self.failure(error),
            }
        }
        if dismiss {
            self.invoice_reversal = None;
        }
    }

    fn import_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Import accounting XML");
        ui.label("Preview one XML file or separate masters and transactions exports together. Include each record only once across both files.");
        for (label, path) in [
            ("First XML file", &mut self.import_path),
            ("Second XML file (optional)", &mut self.import_second_path),
        ] {
            ui.horizontal(|ui| {
                ui.label(label);
                if ui
                    .add(egui::TextEdit::singleline(path).desired_width(420.0))
                    .changed()
                {
                    self.import_preview = None;
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("Opening date (if included)");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.import_opening_date)
                        .hint_text("YYYY-MM-DD"),
                )
                .changed()
            {
                self.import_preview = None;
            }
        });
        if ui.button("Preview XML").clicked() {
            self.import_preview = None;
            let result = (|| -> balance::Result<ImportPlan> {
                use std::io::Read;
                let mut remaining = MAX_XML_IMPORT_BYTES;
                let mut files = Vec::new();
                for path in [&self.import_path, &self.import_second_path]
                    .map(|path| path.trim())
                    .into_iter()
                    .filter(|path| !path.is_empty())
                {
                    let read_error =
                        |e| balance::Error::Validation(format!("Cannot read XML file {path}: {e}"));
                    let metadata = std::fs::metadata(path).map_err(read_error)?;
                    if !metadata.is_file() {
                        return Err(balance::Error::Validation(format!(
                            "Choose a regular XML file: {path}"
                        )));
                    }
                    let size_error = || {
                        balance::Error::Validation(
                            "The XML files exceed the combined 50 MiB import limit".into(),
                        )
                    };
                    if metadata.len() > remaining as u64 {
                        return Err(size_error());
                    }
                    let mut bytes = Vec::new();
                    std::fs::File::open(path)
                        .map_err(read_error)?
                        .take(remaining as u64 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(read_error)?;
                    remaining = remaining.checked_sub(bytes.len()).ok_or_else(size_error)?;
                    files.push(bytes);
                }
                let inputs = files.iter().map(Vec::as_slice).collect::<Vec<_>>();
                self.store.preview_xml_import_batch(
                    company,
                    &inputs,
                    Some(&self.import_opening_date),
                )
            })();
            match result {
                Ok(plan) => {
                    self.import_preview = Some(plan);
                    self.message.clear();
                }
                Err(e) => self.failure(e),
            }
        }
        if let Some(plan) = &self.import_preview {
            ui.label(format!(
                "Preview: {} account groups, {} ledgers, {} opening balances, {} opening bills, {} opening advances, {} opening amounts on account, {} vouchers, {} invoices, {} settlements, {} bill-linked notes, {} advances, {} advance applications, {} named credits, {} credit applications, {} credit refunds, {} units, {} stock groups, {} locations, {} items, {} opening quantities",
                plan.group_count(),
                plan.ledger_count(),
                plan.opening_count(),
                plan.opening_bill_count(),
                plan.opening_advance_count(),
                plan.opening_on_account_count(),
                plan.voucher_count(),
                plan.invoice_count(),
                plan.settlement_count(),
                plan.bill_note_count(),
                plan.advance_count(),
                plan.advance_application_count(),
                plan.note_credit_count(),
                plan.credit_application_count(),
                plan.note_refund_count(),
                plan.unit_count(),
                plan.stock_group_count(),
                plan.location_count(),
                plan.item_count(),
                plan.stock_opening_count()
            ));
            let (valued_openings, value) = plan.opening_stock_value();
            if valued_openings > 0 {
                ui.label(format!(
                    "{valued_openings} opening stock receipts with acquisition costs totalling {}",
                    money(value)
                ));
            }
            if plan.issues().is_empty() {
                ui.label("Preview found no issues. Review the limitations below before importing.");
            } else {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("{} issue(s) prevent import:", plan.issues().len()),
                );
                for issue in plan.issues().iter().take(100) {
                    ui.label(issue);
                }
                if plan.issues().len() > 100 {
                    ui.label("Additional issues are not shown.");
                }
            }
            let can_apply = plan.company_id() == company && plan.can_apply();
            if ui
                .add_enabled(can_apply, egui::Button::new("Import previewed records"))
                .clicked()
            {
                match self.store.apply_xml_import(plan) {
                    Ok(counts) => {
                        self.message = format!(
                            "Imported {} account groups, {} ledgers, {} opening bills, {} opening advances, {} opening amounts on account, {} vouchers, {} invoices, {} settlements, {} bill-linked notes, {} advances, {} advance applications, {} named credits, {} credit applications, {} credit refunds, {} units, {} stock groups, {} locations and {} items",
                            counts.groups, counts.ledgers, counts.opening_bills, counts.opening_advances, counts.opening_on_account, counts.vouchers, counts.invoices, counts.settlements, counts.bill_notes, counts.advances, counts.advance_applications, counts.note_credits, counts.credit_applications, counts.note_refunds, counts.units, counts.stock_groups, counts.locations, counts.items
                        );
                        self.import_preview = None;
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
        ui.small("This initial importer handles simple groups, ledgers, selected contact details, opening balances and bill details, accounting vouchers, simple units, stock groups, stock locations with parent links, stock items, restricted item and non-stock invoices, and simple receipts or payments against invoices. Opening bills retain due dates or whole-day credit periods. Named opening advances retain their original dates and can fund later invoices. Balance records the difference between the named opening entries and the ledger opening total on account. Opening stock can include locations, batches, manufacture dates and reconciled acquisition costs. New bills and applications of named advances or note credits must add up to the party amount on a sales or purchase transaction. Financial notes can reduce open bills or leave named credit for later invoices. Item invoices require one accounting allocation per item. Stock allocations inside a voucher ledger line must add up to that posting. Additional ledger postings become unclassified service, charge or deduction lines. Separate fixed or percentage item discounts must match the source amounts; combined discounts and fixed discounts across multiple stock allocations are unsupported. Location and batch allocations must agree with item quantities and rounded amounts. Receipts and payments require one party and one cash or bank asset ledger. A voucher can contain bill settlements, note credit refunds, named advances and on-account amounts. Named advances retain a due date or whole-day credit period. Tax details, expiry dates, invoice batch dates, conflicting location hierarchies, credit terms on on-account amounts and unsupported fields block import. Import has not been verified against representative exports.");
    }
    fn bank_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Bank clearance");
        ui.label("Manually record clearance dates for postings in a cash or bank asset ledger. Compare these with your bank statement; a cleared balance is calculated from marked book entries, not imported from the bank.");
        let ledgers = match self.store.ledgers(company) {
            Ok(values) => values
                .into_iter()
                .filter(|ledger| ledger.category == Category::Asset)
                .collect::<Vec<_>>(),
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let before = self.bank_ledger;
        pick_account(
            ui,
            "bank-ledger",
            "Asset ledger",
            &ledgers,
            &mut self.bank_ledger,
            false,
        );
        if before != self.bank_ledger {
            self.bank_posting = None;
            self.bank_csv_preview = None;
            self.bank_statement_selection = None;
            self.bank_opening_date.clear();
            self.bank_opening_amount.clear();
            self.bank_opening_reason.clear();
        }
        ui.horizontal(|ui| {
            ui.label("As of");
            ui.add(egui::TextEdit::singleline(&mut self.bank_as_of).hint_text("YYYY-MM-DD"));
        });
        let Some(ledger) = self.bank_ledger else {
            ui.label("Select an asset ledger.");
            return;
        };
        if self.bank_as_of.trim().is_empty() {
            ui.label("Enter an as-of date.");
            return;
        }
        let report = match self.store.bank_report(company, ledger, &self.bank_as_of) {
            Ok(report) => report,
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                return;
            }
        };
        ui.label(format!(
            "Book balance: {}    Cleared postings: {}    Uncleared difference: {}",
            money(report.book_balance),
            money(report.cleared_balance),
            money(report.uncleared)
        ));
        match self
            .store
            .statement_balance(company, ledger, &self.bank_as_of)
        {
            Ok(Some(statement)) => ui.label(format!(
                "Imported statement balance: {}    Book less statement: {} ({} entries since {})",
                money(statement.ending_balance),
                money(report.book_balance - statement.ending_balance),
                statement.entry_count,
                statement.opening.start_date
            )),
            Ok(None) => ui.label(
                "No statement balance is available for this date; set an opening balance first.",
            ),
            Err(e) => ui.colored_label(ui.visuals().error_fg_color, e.to_string()),
        };
        ui.small("The statement balance uses only imported entries and the entered opening; verify coverage against the bank. It is not independently verified.");
        ui.small("Balances are debit minus credit. Past reports use the current clearance dates; revisions do not reconstruct past states.");
        egui::Grid::new("bank-postings")
            .striped(true)
            .show(ui, |ui| {
                for title in [
                    "Select",
                    "Date",
                    "Voucher",
                    "Reference",
                    "Narration",
                    "Amount",
                    "Cleared on",
                ] {
                    ui.strong(title);
                }
                ui.end_row();
                for entry in &report.entries {
                    ui.selectable_value(&mut self.bank_posting, Some(entry.posting_id), "Select");
                    ui.label(&entry.date);
                    ui.label(entry.voucher_id.to_string());
                    ui.label(&entry.reference);
                    ui.label(&entry.narration);
                    ui.label(format_amount(entry.amount));
                    ui.label(entry.cleared_on.as_deref().unwrap_or("—"));
                    ui.end_row();
                }
            });
        let selected_posting = self
            .bank_posting
            .filter(|id| report.entries.iter().any(|entry| entry.posting_id == *id));
        self.bank_statement_page(ui, company, ledger, selected_posting, &report.entries);
        let Some(entry) = report
            .entries
            .iter()
            .find(|entry| Some(entry.posting_id) == self.bank_posting)
        else {
            return;
        };
        ui.separator();
        ui.label(format!(
            "Selected posting {} in voucher {}",
            entry.posting_id, entry.voucher_id
        ));
        ui.horizontal(|ui| {
            ui.label("Clearance date");
            ui.add(
                egui::TextEdit::singleline(&mut self.bank_clearance_date).hint_text("YYYY-MM-DD"),
            );
            ui.label("Reason");
            ui.text_edit_singleline(&mut self.bank_reason);
        });
        ui.horizontal(|ui| {
            if ui.button("Set clearance date").clicked() {
                match self.store.set_bank_clearance(
                    company,
                    ledger,
                    entry.posting_id,
                    Some(self.bank_clearance_date.trim()),
                    &self.bank_reason,
                ) {
                    Ok(()) => {
                        self.bank_reason.clear();
                        self.message = "Clearance saved".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
            if ui
                .add_enabled(
                    entry.cleared_on.is_some(),
                    egui::Button::new("Remove clearance"),
                )
                .clicked()
            {
                match self.store.set_bank_clearance(
                    company,
                    ledger,
                    entry.posting_id,
                    None,
                    &self.bank_reason,
                ) {
                    Ok(()) => {
                        self.bank_reason.clear();
                        self.message = "Clearance removed".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        ui.small("Remove a clearance before editing its voucher. The clearance history remains attached to the voucher.");
        egui::CollapsingHeader::new("Voucher clearance history")
            .id_salt(("bank-history", entry.voucher_id))
            .show(ui, |ui| {
                match self.store.clearance_events(company, entry.voucher_id) {
                    Ok(events) if events.is_empty() => {
                        ui.label("No clearance changes.");
                    }
                    Ok(events) => {
                        for event in events {
                            ui.label(format!(
                                "{} UTC — posting {} / {} / {} / voucher date {}: {} → {} — {}",
                                event.changed_at,
                                event.posting_id,
                                event.ledger,
                                format_amount(event.amount),
                                event.voucher_date,
                                event.previous_date.as_deref().unwrap_or("not cleared"),
                                event.new_date.as_deref().unwrap_or("not cleared"),
                                event.reason
                            ));
                        }
                    }
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                    }
                }
            });
    }
    fn bank_statement_page(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        ledger: i64,
        posting: Option<i64>,
        book_entries: &[BankEntry],
    ) {
        ui.separator();
        crate::theme::section(ui, "Statement entries");
        ui.label("Set the balance immediately before the start date; imported entries on that date are included. Positive is a debit balance, negative a credit balance.");
        let opening = match self.store.statement_opening(company, ledger) {
            Ok(opening) => opening,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        if let Some(current) = &opening {
            ui.label(format!(
                "Current opening before {}: {}",
                current.start_date,
                format_amount(current.amount)
            ));
            if ui.button("Load current statement opening").clicked() {
                self.bank_opening_date = current.start_date.clone();
                self.bank_opening_amount = format_amount(current.amount);
            }
        }
        ui.horizontal(|ui| {
            ui.label("Start date");
            ui.add(egui::TextEdit::singleline(&mut self.bank_opening_date).hint_text("YYYY-MM-DD"));
            ui.label("Opening amount");
            ui.text_edit_singleline(&mut self.bank_opening_amount);
            ui.label("Reason");
            ui.text_edit_singleline(&mut self.bank_opening_reason);
        });
        ui.horizontal(|ui| {
            if ui.button("Save statement opening").clicked() {
                let result = (|| -> balance::Result<()> {
                    let amount = parse_signed_amount(&self.bank_opening_amount)?;
                    self.store.set_statement_opening(
                        company,
                        ledger,
                        Some((&self.bank_opening_date, amount)),
                        &self.bank_opening_reason,
                    )
                })();
                match result {
                    Ok(()) => {
                        self.bank_opening_reason.clear();
                        self.message = "Statement opening saved".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
            if ui
                .add_enabled(
                    opening.is_some(),
                    egui::Button::new("Remove statement opening"),
                )
                .clicked()
            {
                match self.store.set_statement_opening(
                    company,
                    ledger,
                    None,
                    &self.bank_opening_reason,
                ) {
                    Ok(()) => {
                        self.bank_opening_date.clear();
                        self.bank_opening_amount.clear();
                        self.bank_opening_reason.clear();
                        self.message = "Statement opening removed".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        egui::CollapsingHeader::new("Statement opening history").show(ui, |ui| {
            match self.store.statement_opening_events(company, ledger) {
                Ok(events) if events.is_empty() => {
                    ui.label("No opening changes.");
                }
                Ok(events) => {
                    for event in events {
                        let previous = event
                            .previous_date
                            .zip(event.previous_amount)
                            .map(|(date, amount)| format!("{date}: {}", format_amount(amount)))
                            .unwrap_or_else(|| "none".into());
                        let next = event
                            .new_date
                            .zip(event.new_amount)
                            .map(|(date, amount)| format!("{date}: {}", format_amount(amount)))
                            .unwrap_or_else(|| "none".into());
                        ui.label(format!(
                            "{} UTC — {} → {} — {}",
                            event.changed_at, previous, next, event.reason
                        ));
                    }
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                }
            }
        });
        ui.small("CSV columns: id,date,description,amount. Dates use YYYY-MM-DD; deposits are positive and withdrawals negative. IDs must be unique for this ledger. Matching requires the exact amount and a statement date on or after the voucher date. A unique candidate is a suggestion; check its details before matching.");
        ui.horizontal(|ui| {
            ui.label("CSV file");
            if ui
                .add(egui::TextEdit::singleline(&mut self.bank_csv_path).desired_width(360.0))
                .changed()
            {
                self.bank_csv_preview = None;
            }
        });
        if ui.button("Preview statement").clicked() {
            self.bank_csv_preview = None;
            let result = (|| -> balance::Result<StatementPlan> {
                let path = std::path::Path::new(self.bank_csv_path.trim());
                let metadata = std::fs::metadata(path).map_err(|e| {
                    balance::Error::Validation(format!("Cannot read statement: {e}"))
                })?;
                if !metadata.is_file() || metadata.len() > 10 * 1024 * 1024 {
                    return Err(balance::Error::Validation(
                        "Choose a file no larger than 10 MiB".into(),
                    ));
                }
                use std::io::Read;
                let file = std::fs::File::open(path).map_err(|e| {
                    balance::Error::Validation(format!("Cannot read statement: {e}"))
                })?;
                let mut bytes = Vec::new();
                file.take(10 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| {
                        balance::Error::Validation(format!("Cannot read statement: {e}"))
                    })?;
                self.store.preview_bank_statement(company, ledger, &bytes)
            })();
            match result {
                Ok(plan) => {
                    self.bank_csv_preview = Some(plan);
                    self.message.clear();
                }
                Err(e) => self.failure(e),
            }
        }
        if let Some(plan) = &self.bank_csv_preview {
            ui.label(format!("Preview: {} entries", plan.row_count()));
            for issue in plan.issues().iter().take(100) {
                ui.colored_label(ui.visuals().error_fg_color, issue);
            }
            if plan.issues().len() > 100 {
                ui.label("Additional issues are not shown.");
            }
            egui::CollapsingHeader::new("Review preview rows").show(ui, |ui| {
                for row in plan.rows().iter().take(100) {
                    ui.label(format!(
                        "{} | {} | {} | {}",
                        row.external_id,
                        row.date,
                        row.description,
                        format_amount(row.amount)
                    ));
                }
                if plan.row_count() > 100 {
                    ui.label("Only the first 100 rows are shown.");
                }
            });
            if ui
                .add_enabled(
                    plan.can_apply() && plan.company() == company && plan.ledger() == ledger,
                    egui::Button::new("Import statement preview"),
                )
                .clicked()
            {
                match self.store.apply_bank_statement(plan) {
                    Ok(count) => {
                        self.message = format!("Imported {count} statement entries");
                        self.bank_csv_preview = None;
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
        let entries = match self
            .store
            .bank_statement_entries(company, ledger, &self.bank_as_of)
        {
            Ok(entries) => entries,
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                return;
            }
        };
        egui::Grid::new("bank-statement")
            .striped(true)
            .show(ui, |ui| {
                for title in [
                    "Select",
                    "ID",
                    "Date",
                    "Description",
                    "Amount",
                    "Matched posting",
                    "Candidates",
                ] {
                    ui.strong(title);
                }
                ui.end_row();
                for entry in &entries {
                    ui.selectable_value(
                        &mut self.bank_statement_selection,
                        Some(entry.id),
                        "Select",
                    );
                    ui.label(&entry.external_id);
                    ui.label(&entry.date);
                    ui.label(&entry.description);
                    ui.label(format_amount(entry.amount));
                    ui.label(
                        entry
                            .matched_posting
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "—".into()),
                    );
                    let candidates = if entry.matched_posting.is_none() {
                        book_entries
                            .iter()
                            .filter(|book| {
                                book.cleared_on.is_none()
                                    && book.amount == entry.amount
                                    && book.date <= entry.date
                            })
                            .count()
                    } else {
                        0
                    };
                    ui.label(if entry.matched_posting.is_some() {
                        "—".into()
                    } else {
                        candidates.to_string()
                    });
                    ui.end_row();
                }
            });
        let Some(entry) = entries
            .iter()
            .find(|entry| Some(entry.id) == self.bank_statement_selection)
        else {
            return;
        };
        if entry.matched_posting.is_none() {
            let mut candidates = book_entries.iter().filter(|book| {
                book.cleared_on.is_none() && book.amount == entry.amount && book.date <= entry.date
            });
            if let Some(candidate) = candidates.next() {
                if candidates.next().is_none()
                    && ui
                        .button(format!(
                            "Select candidate posting {} (voucher {})",
                            candidate.posting_id, candidate.voucher_id
                        ))
                        .clicked()
                {
                    self.bank_posting = Some(candidate.posting_id);
                }
            }
        }
        ui.horizontal(|ui| {
            ui.label("Match reason");
            ui.text_edit_singleline(&mut self.bank_match_reason);
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    entry.matched_posting.is_none() && posting.is_some(),
                    egui::Button::new("Match selected posting"),
                )
                .clicked()
            {
                if let Some(posting) = posting {
                    match self.store.match_bank_statement(
                        company,
                        ledger,
                        entry.id,
                        posting,
                        &self.bank_match_reason,
                    ) {
                        Ok(()) => {
                            self.bank_match_reason.clear();
                            self.message = "Statement entry matched".into();
                        }
                        Err(e) => self.failure(e),
                    }
                }
            }
            if ui
                .add_enabled(
                    entry.matched_posting.is_some(),
                    egui::Button::new("Remove statement match"),
                )
                .clicked()
            {
                match self.store.unmatch_bank_statement(
                    company,
                    ledger,
                    entry.id,
                    &self.bank_match_reason,
                ) {
                    Ok(()) => {
                        self.bank_match_reason.clear();
                        self.message = "Statement match removed".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        egui::CollapsingHeader::new("Statement match history")
            .id_salt(("statement-match-history", entry.id))
            .show(ui, |ui| {
                match self.store.bank_match_events(company, entry.id) {
                    Ok(events) if events.is_empty() => {
                        ui.label("No matching changes.");
                    }
                    Ok(events) => {
                        for event in events {
                            ui.label(format!(
                                "{} UTC — {} voucher {} posting {} — {}",
                                event.changed_at,
                                event.action,
                                event.voucher_id,
                                event.posting_id,
                                event.reason
                            ));
                        }
                    }
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                    }
                }
            });
    }
    fn invoice_register_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Sales and purchase register");
        self.report_export_path_field(ui);
        ui.small("The date filter applies to original invoices. The report shows original totals and all recorded reversals, including future dates. Use Outstanding for balances on a specific date.");
        let ledgers = match self.store.ledgers(company) {
            Ok(value) => value,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let parties: Vec<_> = ledgers
            .into_iter()
            .filter(|ledger| matches!(ledger.category, Category::Asset | Category::Liability))
            .collect();
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(
                egui::TextEdit::singleline(&mut self.invoice_register_from)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            ui.label("To");
            ui.add(
                egui::TextEdit::singleline(&mut self.invoice_register_to)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            egui::ComboBox::from_label("Type")
                .selected_text(
                    self.invoice_register_kind
                        .map(|k| k.as_str())
                        .unwrap_or("All"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.invoice_register_kind, None, "All");
                    for kind in InvoiceKind::ALL {
                        ui.selectable_value(
                            &mut self.invoice_register_kind,
                            Some(kind),
                            kind.as_str(),
                        );
                    }
                });
        });
        pick_account(
            ui,
            "invoice-register-party",
            "Party (optional)",
            &parties,
            &mut self.invoice_register_party,
            true,
        );
        ui.horizontal(|ui| {
            ui.label("Search party, reference, or invoice ID");
            ui.text_edit_singleline(&mut self.invoice_register_search);
        });
        let from = (!self.invoice_register_from.trim().is_empty())
            .then_some(self.invoice_register_from.trim());
        let to = (!self.invoice_register_to.trim().is_empty())
            .then_some(self.invoice_register_to.trim());
        match self.store.invoice_register(
            company,
            from,
            to,
            self.invoice_register_kind,
            self.invoice_register_party,
            &self.invoice_register_search,
        ) {
            Ok(rows) => {
                if ui.button("Export invoice register as CSV").clicked() {
                    match self.store.export_invoice_register_csv(
                        company,
                        from,
                        to,
                        self.invoice_register_kind,
                        self.invoice_register_party,
                        &self.invoice_register_search,
                        self.report_export_path.trim(),
                    ) {
                        Ok(()) => self.message = "Invoice register exported".into(),
                        Err(e) => self.failure(e),
                    }
                }
                let mut sales = 0_i128;
                let mut purchases = 0_i128;
                egui::Grid::new("invoice-register")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in [
                            "Date",
                            "Type",
                            "Invoice",
                            "Party",
                            "Reference / bill",
                            "External invoice",
                            "Due",
                            "Original total",
                            "Reversed on",
                            "Settled",
                            "Advances",
                            "Credits",
                            "Outstanding",
                        ] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for invoice in &rows {
                            match invoice.kind {
                                InvoiceKind::Sale => sales += invoice.total,
                                InvoiceKind::Purchase => purchases += invoice.total,
                            }
                            ui.label(&invoice.date);
                            ui.label(invoice.document_type());
                            ui.label(invoice.id.to_string());
                            ui.label(&invoice.party);
                            ui.label(reference_with_bill(
                                &invoice.reference,
                                &invoice.bill_reference,
                            ));
                            ui.label(
                                external_invoice_label(&invoice.external_reference)
                                    .unwrap_or_default(),
                            );
                            ui.label(invoice.due_date.as_deref().unwrap_or("—"));
                            ui.label(money(invoice.total));
                            ui.label(
                                invoice
                                    .reversal
                                    .as_ref()
                                    .map(|r| r.date.as_str())
                                    .unwrap_or("—"),
                            );
                            ui.label(money(invoice.settled));
                            ui.label(money(invoice.advance_applied));
                            ui.label(money(invoice.credited));
                            ui.label(money(invoice.outstanding));
                            ui.end_row();
                        }
                    });
                ui.label(format!(
                    "{} invoices · Original sales {} · Original purchases {}",
                    rows.len(),
                    money(sales),
                    money(purchases)
                ));
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
    }
    fn outstanding_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Outstanding invoices");
        self.report_export_path_field(ui);
        ui.label("Age uses the invoice or original opening bill date. Overdue days use the due date. Past reports use the current bill details.");
        ui.horizontal(|ui| {
            ui.label("As of");
            ui.add(
                egui::TextEdit::singleline(&mut self.outstanding_date)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(120.0),
            );
        });
        if self.outstanding_date.trim().is_empty() {
            ui.label("Enter an as-of date.");
            return;
        }
        self.refund_balances_panel(ui, company);
        self.bill_note_credits_panel(ui, company);
        match self
            .store
            .outstanding_invoices(company, &self.outstanding_date)
        {
            Ok(rows) => {
                if ui.button("Export outstanding invoices as CSV").clicked() {
                    match self.store.export_outstanding_csv(
                        company,
                        self.outstanding_date.trim(),
                        self.report_export_path.trim(),
                    ) {
                        Ok(()) => self.message = "Outstanding invoices exported".into(),
                        Err(e) => self.failure(e),
                    }
                }
                let mut receivable = 0_i128;
                let mut payable = 0_i128;
                let mut receivable_age = [0_i128; 4];
                let mut payable_age = [0_i128; 4];
                let mut receivable_due = [0_i128; 6];
                let mut payable_due = [0_i128; 6];
                egui::Grid::new("outstanding").striped(true).show(ui, |ui| {
                    for title in [
                        "Type",
                        "Party",
                        "Date",
                        "Due date",
                        "Reference / bill",
                        "Total",
                        "Settled",
                        "Advances",
                        "Credits",
                        "Outstanding",
                        "Age (days)",
                        "Overdue (days)",
                    ] {
                        ui.strong(title);
                    }
                    ui.end_row();
                    for row in rows {
                        let bucket = match row.age_days {
                            0..=30 => 0,
                            31..=60 => 1,
                            61..=90 => 2,
                            _ => 3,
                        };
                        let due_bucket = match row.overdue_days {
                            None => 0,
                            Some(0) => 1,
                            Some(1..=30) => 2,
                            Some(31..=60) => 3,
                            Some(61..=90) => 4,
                            Some(_) => 5,
                        };
                        if row.kind == InvoiceKind::Sale {
                            receivable += row.outstanding;
                            receivable_age[bucket] += row.outstanding;
                            receivable_due[due_bucket] += row.outstanding;
                        } else {
                            payable += row.outstanding;
                            payable_age[bucket] += row.outstanding;
                            payable_due[due_bucket] += row.outstanding;
                        }
                        if let Some(date) = &row.opening_date {
                            ui.label(balance::opening_bills::label(row.kind))
                                .on_hover_text(format!("Posted to the books on {date}"));
                        } else {
                            ui.label(row.entry_mode.label(row.kind));
                        }
                        ui.label(row.party);
                        ui.label(row.date);
                        ui.label(row.due_date.as_deref().unwrap_or("—"));
                        ui.label(reference_with_bill(&row.reference, &row.bill_reference));
                        ui.label(money(row.total));
                        ui.label(money(row.settled));
                        ui.label(money(row.advance_applied));
                        ui.label(money(row.credited));
                        ui.label(money(row.outstanding));
                        ui.label(row.age_days.to_string());
                        ui.label(
                            row.overdue_days
                                .map(|days| days.to_string())
                                .unwrap_or_else(|| "—".into()),
                        );
                        ui.end_row();
                    }
                });
                ui.label(format!(
                    "Receivable: {}    Payable: {}",
                    money(receivable),
                    money(payable)
                ));
                crate::theme::section(ui, "Outstanding by party");
                ui.small("Opening amounts on account appear separately from named bills. Overdue totals require a due date. Credit limits are current and informational.");
                if ui.button("Export party outstanding as CSV").clicked() {
                    match self.store.export_party_outstanding_csv(
                        company,
                        self.outstanding_date.trim(),
                        self.report_export_path.trim(),
                    ) {
                        Ok(()) => self.message = "Party outstanding report exported".into(),
                        Err(e) => self.failure(e),
                    }
                }
                match self
                    .store
                    .party_outstanding(company, self.outstanding_date.trim())
                {
                    Ok(parties) => {
                        egui::Grid::new("outstanding-parties")
                            .striped(true)
                            .show(ui, |ui| {
                                for label in [
                                    "Party",
                                    "Invoices",
                                    "Receivable",
                                    "Payable",
                                    "Overdue receivable",
                                    "Overdue payable",
                                    "Credit limit",
                                    "Opening on account",
                                ] {
                                    ui.strong(label);
                                }
                                ui.end_row();
                                for party in parties {
                                    let limit = self
                                        .store
                                        .ledger_terms(company, party.party_ledger_id)
                                        .map(|terms| {
                                            terms
                                                .credit_limit
                                                .map(format_amount)
                                                .unwrap_or_else(|| "—".into())
                                        })
                                        .unwrap_or_else(|_| "Unavailable".into());
                                    ui.label(party.party);
                                    ui.label(party.invoices.to_string());
                                    ui.label(money(party.receivable));
                                    ui.label(money(party.payable));
                                    ui.label(money(party.overdue_receivable));
                                    ui.label(money(party.overdue_payable));
                                    ui.label(limit);
                                    ui.label(match party.opening_on_account {
                                        0 => "—".into(),
                                        amount if amount > 0 => format!("{} Dr", money(amount)),
                                        amount => format!("{} Cr", money(-amount)),
                                    });
                                    ui.end_row();
                                }
                            });
                    }
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                    }
                };
                crate::theme::section(ui, "Age from invoice date");
                egui::Grid::new("outstanding-age")
                    .striped(true)
                    .show(ui, |ui| {
                        for title in ["", "0–30 days", "31–60 days", "61–90 days", "Over 90 days"]
                        {
                            ui.strong(title);
                        }
                        ui.end_row();
                        ui.label("Receivable");
                        for amount in receivable_age {
                            ui.label(money(amount));
                        }
                        ui.end_row();
                        ui.label("Payable");
                        for amount in payable_age {
                            ui.label(money(amount));
                        }
                        ui.end_row();
                    });
                crate::theme::section(ui, "Age from due date");
                ui.small("Current includes invoices due today or later; invoices without a due date are shown separately.");
                egui::Grid::new("outstanding-due-age")
                    .striped(true)
                    .show(ui, |ui| {
                        for title in [
                            "",
                            "No due date",
                            "Current",
                            "1–30 days",
                            "31–60 days",
                            "61–90 days",
                            "Over 90 days",
                        ] {
                            ui.strong(title);
                        }
                        ui.end_row();
                        ui.label("Receivable");
                        for amount in receivable_due {
                            ui.label(money(amount));
                        }
                        ui.end_row();
                        ui.label("Payable");
                        for amount in payable_due {
                            ui.label(money(amount));
                        }
                        ui.end_row();
                    });
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
    }
    fn day_book_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Day book");
        self.report_export_path_field(ui);
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(
                egui::TextEdit::singleline(&mut self.day_from)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            ui.label("To");
            ui.add(
                egui::TextEdit::singleline(&mut self.day_to)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            egui::ComboBox::from_label("Type")
                .selected_text(self.day_kind.map(|k| k.as_str()).unwrap_or("All"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.day_kind, None, "All");
                    for kind in VoucherKind::ALL {
                        ui.selectable_value(&mut self.day_kind, Some(kind), kind.as_str());
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Search reference, narration or ID");
            ui.text_edit_singleline(&mut self.day_search);
        });
        let from = if self.day_from.trim().is_empty() {
            None
        } else {
            Some(self.day_from.trim())
        };
        let to = if self.day_to.trim().is_empty() {
            None
        } else {
            Some(self.day_to.trim())
        };
        match self
            .store
            .day_book(company, from, to, self.day_kind, &self.day_search)
        {
            Ok(entries) => {
                if ui.button("Export day book as CSV").clicked() {
                    match self.store.export_day_book_csv(
                        company,
                        from,
                        to,
                        self.day_kind,
                        &self.day_search,
                        self.report_export_path.trim(),
                    ) {
                        Ok(()) => self.message = "Day book exported".into(),
                        Err(e) => self.failure(e),
                    }
                }
                ui.label(format!("{} voucher(s)", entries.len()));
                egui::Grid::new("day-book").striped(true).show(ui, |ui| {
                    for name in [
                        "Date",
                        "ID",
                        "Type",
                        "Reference",
                        "Narration",
                        "Debit",
                        "Credit",
                    ] {
                        ui.strong(name);
                    }
                    ui.end_row();
                    for entry in entries {
                        ui.label(entry.voucher.date);
                        ui.label(entry.voucher.id.to_string());
                        ui.label(entry.voucher.kind.as_str());
                        ui.label(entry.voucher.reference);
                        ui.label(entry.voucher.narration);
                        ui.label(money(entry.debit_total));
                        ui.label(money(entry.credit_total));
                        ui.end_row();
                    }
                });
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
    }
    fn report_export_path_field(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("New report CSV file");
            ui.add(
                egui::TextEdit::singleline(&mut self.report_export_path)
                    .hint_text("Absolute path ending in .csv")
                    .desired_width(350.0),
            );
        });
        ui.small("Exports include the current report filters and never overwrite existing files. Day-book, invoice-register, outstanding, stock and trial-balance files include a context row; ledger files include opening and closing rows. Potential spreadsheet formulas in text are prefixed with an apostrophe.");
    }
    fn backup_page(&mut self, ui: &mut egui::Ui) {
        crate::theme::section(ui, "Back up data");
        ui.label("Create a snapshot of all companies, ledgers, vouchers, invoices, stock records and saved revisions in this database.");
        ui.label("Choose a new file path. Existing files are not overwritten.");
        ui.horizontal(|ui| {
            ui.label("New backup file");
            ui.add(egui::TextEdit::singleline(&mut self.backup_path).desired_width(420.0));
        });
        if ui.button("Create backup").clicked() {
            let path = self.backup_path.trim();
            if path.is_empty() {
                self.message = "Enter a backup file path".into();
            } else {
                match self.store.backup_to(path) {
                    Ok(()) => self.message = "Backup created".into(),
                    Err(e) => self.failure(e),
                }
            }
        }
        ui.small("If backup creation fails, an incomplete file may remain at the selected path.");
        ui.separator();
        crate::theme::section(ui, "Restore to a new file");
        ui.label("Choose an existing backup and a new destination. The current database stays open and is not replaced. The new copy is checked and upgraded before use.");
        ui.horizontal(|ui| {
            ui.label("Backup file");
            ui.add(egui::TextEdit::singleline(&mut self.restore_source).desired_width(420.0));
        });
        ui.horizontal(|ui| {
            ui.label("New database file");
            ui.add(egui::TextEdit::singleline(&mut self.restore_destination).desired_width(420.0));
        });
        if ui.button("Restore copy").clicked() {
            match Store::restore_backup(self.restore_source.trim(), self.restore_destination.trim())
            {
                Ok(()) => self.message = "Backup restored to a new database file".into(),
                Err(e) => self.failure(e),
            }
        }
        ui.small("After a successful restore, open the copy by launching Balance with --database followed by its absolute path. If restoration fails, an incomplete destination file may remain.");
    }
}
fn pick_batch(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    batches: &[StockBatch],
    selected: &mut Option<i64>,
) {
    ui.label("Batch");
    let text = batches
        .iter()
        .find(|batch| Some(batch.id) == *selected)
        .map(|batch| batch.name.as_str())
        .unwrap_or("Unbatched");
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, "Unbatched");
            for batch in batches {
                ui.selectable_value(selected, Some(batch.id), &batch.name);
            }
        });
}
fn pick_price_list(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    lists: &[PriceList],
    selected: &mut Option<i64>,
) {
    ui.label("Price list");
    let name = lists
        .iter()
        .find(|list| Some(list.id) == *selected)
        .map(|list| list.name.as_str())
        .unwrap_or("Select list");
    egui::ComboBox::from_id_salt(id)
        .selected_text(name)
        .show_ui(ui, |ui| {
            for list in lists {
                ui.selectable_value(selected, Some(list.id), &list.name);
            }
        });
}
fn pick_location(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    label: &str,
    locations: &[StockLocation],
    selected: &mut Option<i64>,
    all: bool,
) {
    ui.label(label);
    let text = locations
        .iter()
        .find(|location| Some(location.id) == *selected)
        .map(|location| location.name.as_str())
        .unwrap_or(if all {
            "All locations"
        } else {
            "Select location"
        });
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .show_ui(ui, |ui| {
            if all {
                ui.selectable_value(selected, None, "All locations");
            }
            for location in locations {
                ui.selectable_value(selected, Some(location.id), &location.name);
            }
        });
}
fn pick_unit(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    units: &[StockUnit],
    simple_only: bool,
    selected: &mut Option<i64>,
) {
    ui.label(label);
    let choices = units
        .iter()
        .filter(|unit| !simple_only || unit.compound.is_none());
    let text = choices
        .clone()
        .find(|unit| Some(unit.id) == *selected)
        .map_or("Select unit", |unit| unit.symbol.as_str());
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .show_ui(ui, |ui| {
            for unit in choices {
                ui.selectable_value(selected, Some(unit.id), &unit.symbol);
            }
        });
}

fn pick_stock(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    label: &str,
    items: &[StockItem],
    selected: &mut Option<i64>,
) {
    ui.label(label);
    let text = items
        .iter()
        .find(|item| Some(item.id) == *selected)
        .map(|item| item.name.as_str())
        .unwrap_or("Select item");
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .show_ui(ui, |ui| {
            for item in items {
                ui.selectable_value(selected, Some(item.id), &item.name);
            }
        });
}
fn pick_account(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    label: &str,
    ledgers: &[Ledger],
    selected: &mut Option<i64>,
    optional: bool,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let text = ledgers
            .iter()
            .find(|ledger| Some(ledger.id) == *selected)
            .map(|ledger| ledger.name.as_str())
            .unwrap_or("Select ledger");
        egui::ComboBox::from_id_salt(id)
            .selected_text(text)
            .show_ui(ui, |ui| {
                if optional {
                    ui.selectable_value(selected, None, "None");
                }
                for ledger in ledgers {
                    ui.selectable_value(selected, Some(ledger.id), &ledger.name);
                }
            });
    });
}
fn discount_description(discount: &InvoiceDiscount, amount: i64) -> String {
    match discount {
        InvoiceDiscount::None => "no discount".into(),
        InvoiceDiscount::Fixed(_) => format!("discount {}", format_amount(amount)),
        InvoiceDiscount::Percent(rate) => format!("discount {}% ({})", rate, format_amount(amount)),
    }
}
fn reference_with_bill(reference: &str, bill_reference: &str) -> String {
    if reference == bill_reference {
        reference.to_owned()
    } else {
        format!("{reference} · Bill {bill_reference}")
    }
}

fn external_invoice_label(reference: &InvoiceExternalReference) -> Option<String> {
    match (&reference.number, &reference.date) {
        (Some(number), Some(date)) => Some(format!("{number} · {date}")),
        (Some(value), None) | (None, Some(value)) => Some(value.clone()),
        (None, None) => None,
    }
}

fn invoice_tax_detail_label(tax: &InvoiceTaxDetail) -> String {
    let mut label = format!("Tax in {} | {}", tax.ledger, format_amount(tax.amount));
    if !tax.source_rates.is_empty() {
        label.push_str(&format!(
            " | Source rates {}",
            format_source_tax_rates(&tax.source_rates)
        ));
    }
    label
}

fn outstanding_bill_choices<'a>(
    invoices: impl IntoIterator<Item = &'a balance::inventory::InvoiceSummary>,
) -> Vec<(BillSettlementTarget, String)> {
    let mut choices = Vec::new();
    for invoice in invoices {
        if invoice.outstanding <= 0 {
            continue;
        }
        let mut add = |split_id, bill_reference: &str, amount| {
            if amount > 0 {
                choices.push((
                    BillSettlementTarget {
                        invoice_id: invoice.id,
                        split_id,
                    },
                    format!(
                        "{} {} - {} ({})",
                        invoice.document_type(),
                        reference_with_bill(&invoice.reference, bill_reference),
                        invoice.party,
                        money(amount)
                    ),
                ));
            }
        };
        if invoice.bill_splits.is_empty() {
            add(None, &invoice.bill_reference, invoice.outstanding);
        } else {
            for bill in &invoice.bill_splits {
                add(Some(bill.id), &bill.reference, bill.outstanding());
            }
        }
    }
    choices
}

fn pick_settlement_bill(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    choices: &[(BillSettlementTarget, String)],
    selection: &mut Option<BillSettlementTarget>,
) {
    let selected = choices
        .iter()
        .find(|(target, _)| Some(*target) == *selection)
        .map(|(_, caption)| caption.as_str())
        .unwrap_or("Select bill");
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (target, caption) in choices {
                ui.selectable_value(selection, Some(*target), caption);
            }
        });
}

fn money(value: i128) -> String {
    let n = value.unsigned_abs();
    format!(
        "{}{}.{:02}",
        if value < 0 { "-" } else { "" },
        n / 100,
        n % 100
    )
}
impl eframe::App for App {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.shell_update(ctx);
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let explicit_database = args.len() != 0;
    let database_path = if let Some(flag) = args.next() {
        if flag != "--database" {
            return Err("Usage: balance [--database <absolute path>]".into());
        }
        let path = args.next().ok_or("Missing database path")?;
        if args.next().is_some() {
            return Err("Usage: balance [--database <absolute path>]".into());
        }
        std::path::PathBuf::from(path)
    } else {
        let dirs = directories::ProjectDirs::from("org", "Balance", "Balance")
            .ok_or("Cannot find application data directory")?;
        std::fs::create_dir_all(dirs.data_local_dir())?;
        dirs.data_local_dir().join("balance.db")
    };
    let store = if explicit_database {
        Store::open_existing(&database_path)?
    } else {
        Store::open(&database_path)?
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Balance")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([1024.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Balance",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(App::new(store, database_path)))
        }),
    )?;
    Ok(())
}
