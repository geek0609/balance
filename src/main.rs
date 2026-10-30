#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use balance::banking::StatementPlan;
use balance::contacts::LedgerProfile;
use balance::exchange::ImportPlan;
use balance::inventory::{
    format_quantity, line_amount, parse_quantity, InvoiceAdjustment, InvoiceDraft, InvoiceKind,
    InvoiceLine, SettlementAllocation, StockItem, StockLocation,
};
use balance::{format_amount, parse_amount, Category, Ledger, Posting, Store, VoucherKind};
use eframe::egui::{self, Color32, RichText};

#[derive(PartialEq)]
enum Page {
    Company,
    Ledger,
    Voucher,
    Register,
    Import,
    Stock,
    Invoice,
    Backup,
    DayBook,
    Outstanding,
    Bank,
    Reports,
}
struct App {
    store: Store,
    database_path: std::path::PathBuf,
    page: Page,
    company: Option<i64>,
    company_name: String,
    ledger_name: String,
    category: Category,
    ledger_group: Option<i64>,
    group_name: String,
    group_parent: Option<i64>,
    profile_ledger: Option<i64>,
    profile_selection: Option<i64>,
    ledger_profile: LedgerProfile,
    profile_original: LedgerProfile,
    opening_ledger: Option<i64>,
    opening_date: String,
    opening_amount: String,
    opening_debit: bool,
    lines: Vec<VoucherLine>,
    editing_voucher: Option<i64>,
    voucher_kind: VoucherKind,
    voucher_reference: String,
    date: String,
    narration: String,
    report_from: String,
    report_to: String,
    register_ledger: Option<i64>,
    register_from: String,
    register_to: String,
    day_from: String,
    day_to: String,
    day_kind: Option<VoucherKind>,
    day_search: String,
    outstanding_date: String,
    bank_ledger: Option<i64>,
    bank_as_of: String,
    bank_posting: Option<i64>,
    bank_clearance_date: String,
    bank_reason: String,
    bank_csv_path: String,
    bank_csv_preview: Option<StatementPlan>,
    bank_statement_selection: Option<i64>,
    bank_match_reason: String,
    import_path: String,
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
    stock_group_name: String,
    stock_group_parent: Option<i64>,
    stock_location_name: String,
    stock_balance_location: Option<i64>,
    adjustment_location: Option<i64>,
    transfer_item: Option<i64>,
    transfer_from: Option<i64>,
    transfer_to: Option<i64>,
    transfer_date: String,
    transfer_quantity: String,
    transfer_reason: String,
    stock_as_of: String,
    stock_register_item: Option<i64>,
    stock_register_from: String,
    stock_register_to: String,
    adjustment_item: Option<i64>,
    adjustment_date: String,
    adjustment_quantity: String,
    adjustment_increase: bool,
    adjustment_reason: String,
    invoice_kind: InvoiceKind,
    invoice_date: String,
    invoice_due_date: String,
    invoice_credit_days: String,
    invoice_reference: String,
    invoice_narration: String,
    invoice_party: Option<i64>,
    invoice_trade: Option<i64>,
    invoice_tax_ledger: Option<i64>,
    invoice_tax_amount: String,
    invoice_lines: Vec<InvoiceLineForm>,
    invoice_adjustments: Vec<InvoiceAdjustmentForm>,
    editing_invoice: Option<i64>,
    settlement_invoice: Option<i64>,
    settlement_bank: Option<i64>,
    settlement_date: String,
    settlement_reference: String,
    settlement_amount: String,
    settlement_extra: Vec<SettlementLineForm>,
    message: String,
}
struct SettlementLineForm {
    invoice: Option<i64>,
    amount: String,
}
struct InvoiceAdjustmentForm {
    ledger: Option<i64>,
    amount: String,
    increase: bool,
    description: String,
}
struct InvoiceLineForm {
    item: Option<i64>,
    location: Option<i64>,
    quantity: String,
    price: String,
}
impl InvoiceLineForm {
    fn new() -> Self {
        Self {
            item: None,
            location: None,
            quantity: String::new(),
            price: String::new(),
        }
    }
}
struct VoucherLine {
    ledger: Option<i64>,
    debit: bool,
    amount: String,
}
impl VoucherLine {
    fn new(debit: bool) -> Self {
        Self {
            ledger: None,
            debit,
            amount: String::new(),
        }
    }
}
impl App {
    fn new(store: Store, database_path: std::path::PathBuf) -> Self {
        Self {
            store,
            database_path,
            page: Page::Company,
            company: None,
            company_name: String::new(),
            ledger_name: String::new(),
            category: Category::Asset,
            ledger_group: None,
            group_name: String::new(),
            group_parent: None,
            profile_ledger: None,
            profile_selection: None,
            ledger_profile: LedgerProfile::default(),
            profile_original: LedgerProfile::default(),
            opening_ledger: None,
            opening_date: String::new(),
            opening_amount: String::new(),
            opening_debit: true,
            lines: vec![VoucherLine::new(true), VoucherLine::new(false)],
            editing_voucher: None,
            voucher_kind: VoucherKind::Journal,
            voucher_reference: String::new(),
            date: String::new(),
            narration: String::new(),
            report_from: String::new(),
            report_to: String::new(),
            register_ledger: None,
            register_from: String::new(),
            register_to: String::new(),
            day_from: String::new(),
            day_to: String::new(),
            day_kind: None,
            day_search: String::new(),
            outstanding_date: String::new(),
            bank_ledger: None,
            bank_as_of: String::new(),
            bank_posting: None,
            bank_clearance_date: String::new(),
            bank_reason: String::new(),
            bank_csv_path: String::new(),
            bank_csv_preview: None,
            bank_statement_selection: None,
            bank_match_reason: String::new(),
            import_path: String::new(),
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
            stock_group_name: String::new(),
            stock_group_parent: None,
            stock_location_name: String::new(),
            stock_balance_location: None,
            adjustment_location: None,
            transfer_item: None,
            transfer_from: None,
            transfer_to: None,
            transfer_date: String::new(),
            transfer_quantity: String::new(),
            transfer_reason: String::new(),
            stock_as_of: String::new(),
            stock_register_item: None,
            stock_register_from: String::new(),
            stock_register_to: String::new(),
            adjustment_item: None,
            adjustment_date: String::new(),
            adjustment_quantity: String::new(),
            adjustment_increase: true,
            adjustment_reason: String::new(),
            invoice_kind: InvoiceKind::Sale,
            invoice_date: String::new(),
            invoice_due_date: String::new(),
            invoice_credit_days: String::new(),
            invoice_reference: String::new(),
            invoice_narration: String::new(),
            invoice_party: None,
            invoice_trade: None,
            invoice_tax_ledger: None,
            invoice_tax_amount: String::new(),
            invoice_lines: vec![InvoiceLineForm::new()],
            invoice_adjustments: Vec::new(),
            editing_invoice: None,
            settlement_invoice: None,
            settlement_bank: None,
            settlement_date: String::new(),
            settlement_reference: String::new(),
            settlement_amount: String::new(),
            settlement_extra: Vec::new(),
            message: String::new(),
        }
    }
    fn failure(&mut self, e: impl std::fmt::Display) {
        self.message = e.to_string();
    }
    fn clear_voucher(&mut self) {
        self.lines = vec![VoucherLine::new(true), VoucherLine::new(false)];
        self.editing_voucher = None;
        self.voucher_kind = VoucherKind::Journal;
        self.voucher_reference.clear();
        self.narration.clear();
    }
    fn reset_company_forms(&mut self) {
        self.clear_voucher();
        self.register_ledger = None;
        self.ledger_group = None;
        self.group_parent = None;
        self.profile_ledger = None;
        self.profile_selection = None;
        self.ledger_profile = LedgerProfile::default();
        self.profile_original = LedgerProfile::default();
        self.opening_ledger = None;
        self.bank_ledger = None;
        self.bank_posting = None;
        self.bank_clearance_date.clear();
        self.bank_reason.clear();
        self.bank_csv_preview = None;
        self.bank_statement_selection = None;
        self.bank_match_reason.clear();
        self.opening_date.clear();
        self.opening_amount.clear();
        self.import_preview = None;
        self.adjustment_item = None;
        self.stock_creation_unit = None;
        self.stock_creation_group = None;
        self.stock_edit_selection = None;
        self.stock_editing = None;
        self.stock_edit_name.clear();
        self.stock_edit_group = None;
        self.stock_edit_description.clear();
        self.stock_group_parent = None;
        self.stock_balance_location = None;
        self.adjustment_location = None;
        self.transfer_item = None;
        self.transfer_from = None;
        self.transfer_to = None;
        self.transfer_date.clear();
        self.transfer_quantity.clear();
        self.transfer_reason.clear();
        self.stock_register_item = None;
        self.invoice_party = None;
        self.invoice_trade = None;
        self.invoice_tax_ledger = None;
        self.invoice_lines = vec![InvoiceLineForm::new()];
        self.invoice_adjustments.clear();
        self.editing_invoice = None;
        self.invoice_reference.clear();
        self.invoice_narration.clear();
        self.invoice_due_date.clear();
        self.invoice_credit_days.clear();
        self.invoice_tax_amount.clear();
        self.settlement_invoice = None;
        self.settlement_bank = None;
        self.settlement_reference.clear();
        self.settlement_amount.clear();
        self.settlement_extra.clear();
    }
    fn company_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Companies");
        ui.label("Create or select a company to work with.");
        let profile_dirty = (self.profile_ledger.is_some()
            && self.ledger_profile != self.profile_original)
            || self.editing_invoice.is_some()
            || self.stock_editing.is_some();
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
    }
    fn ledger_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Ledgers");
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
                ui.label(format!("{}  —  {}", ledger.name, group));
            }
        }
        ui.separator();
        ui.heading("Opening balances");
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
        }
        if ui.button("Load existing opening balance").clicked() {
            if let Some(id) = self.opening_ledger {
                match self.store.opening_balance(company, id) {
                    Ok(Some(value)) => {
                        self.opening_date = value.date;
                        self.opening_debit = value.amount > 0;
                        self.opening_amount = format_amount(value.amount)
                            .trim_start_matches('-')
                            .to_owned();
                    }
                    Ok(None) => {
                        self.opening_date.clear();
                        self.opening_amount.clear();
                        self.opening_debit = true;
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
        if ui.button("Save opening balance").clicked() {
            let result = (|| -> balance::Result<i64> {
                let ledger = self
                    .opening_ledger
                    .ok_or_else(|| balance::Error::Validation("Select a ledger".into()))?;
                let amount = parse_amount(&self.opening_amount)?;
                self.store.set_opening_balance(
                    company,
                    ledger,
                    &self.opening_date,
                    if self.opening_debit { amount } else { -amount },
                )
            })();
            match result {
                Ok(id) => self.message = format!("Opening balance saved (voucher {id})"),
                Err(e) => self.failure(e),
            }
        }
        ui.small("Opening balances must be non-zero. Clearing one is not yet supported.");
        ui.separator();
        ui.heading("Ledger details");
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
                            ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                        }
                    }
                });
        }
        ui.separator();
        ui.heading("Account groups");
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
        ui.heading(match self.editing_voucher {
            Some(id) => format!("Edit voucher {id}"),
            None => "Journal voucher".into(),
        });
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
        let submit = ui
            .button(if self.editing_voucher.is_some() {
                "Save changes"
            } else {
                "Post voucher"
            })
            .clicked()
            || ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::Enter));
        if self.editing_voucher.is_some() && ui.button("Cancel edit").clicked() {
            self.clear_voucher();
            self.message.clear();
        }
        if submit {
            let result = (|| -> balance::Result<i64> {
                let mut postings = Vec::with_capacity(self.lines.len());
                for line in &self.lines {
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
                }
                if let Some(id) = self.editing_voucher {
                    self.store.update_voucher_with_reference(
                        company,
                        id,
                        self.voucher_kind,
                        &self.voucher_reference,
                        &self.date,
                        &self.narration,
                        &postings,
                    )?;
                    Ok(id)
                } else {
                    self.store.post_voucher_with_reference(
                        company,
                        self.voucher_kind,
                        &self.voucher_reference,
                        &self.date,
                        &self.narration,
                        &postings,
                    )
                }
            })();
            match result {
                Ok(id) => {
                    self.message = format!("Voucher {id} saved");
                    self.clear_voucher();
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        ui.heading("Voucher history");
        match self.store.vouchers(company) {
            Ok(vouchers) => {
                for voucher in vouchers {
                    let invoice_backed = self
                        .store
                        .is_invoice_voucher(company, voucher.id)
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
                            .add_enabled(!invoice_backed, egui::Button::new("Edit"))
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
                                .map(|p| VoucherLine {
                                    ledger: Some(p.ledger_id),
                                    debit: p.amount > 0,
                                    amount: format_amount(p.amount)
                                        .trim_start_matches('-')
                                        .to_owned(),
                                })
                                .collect();
                            self.message.clear();
                        }
                        if invoice_backed {
                            ui.small("Linked postings cannot be edited here");
                        }
                    });
                    for p in &voucher.postings {
                        let name = ledgers
                            .iter()
                            .find(|l| l.id == p.ledger_id)
                            .map(|l| l.name.as_str())
                            .unwrap_or("Unknown ledger");
                        ui.label(format!(
                            "  {}  {}  {}",
                            name,
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
                                        for p in revision.voucher.postings {
                                            let name = ledgers
                                                .iter()
                                                .find(|l| l.id == p.ledger_id)
                                                .map(|l| l.name.as_str())
                                                .unwrap_or("Unknown ledger");
                                            ui.label(format!(
                                                "  {}  {}  {}",
                                                name,
                                                if p.amount > 0 { "Dr" } else { "Cr" },
                                                format_amount(p.amount).trim_start_matches('-')
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                                }
                            }
                        });
                }
            }
            Err(e) => self.failure(e),
        }
    }
    fn reports_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Financial reports");
        ui.label("Dates are inclusive. Leave a field blank for an open boundary.");
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                return;
            }
        };
        ui.heading("Trial balance");
        ui.label("As of the end date; debit balances are positive, credits negative.");
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
        ui.separator();
        ui.heading("Profit and loss");
        ui.label(format!(
            "Income: {}    Expenses: {}    Net profit: {}",
            money(report.income),
            money(report.expenses),
            money(report.profit)
        ));
        ui.heading("Balance sheet");
        ui.label(format!(
            "Assets: {}    Liabilities: {}    Equity including profit: {}",
            money(report.assets),
            money(report.liabilities),
            money(report.equity)
        ));
        ui.small("The balance sheet includes cumulative profit through the end date. Opening balances, tax, and statutory adjustments are not implemented.");
    }
    fn register_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Ledger activity");
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                return;
            }
        };
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
        ui.heading("Units and stock groups");
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
        ui.heading("Stock locations");
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.stock_location_name);
            if ui.button("Create location").clicked() {
                match self
                    .store
                    .create_stock_location(company, &self.stock_location_name)
                {
                    Ok(id) => {
                        self.stock_location_name.clear();
                        self.transfer_to = Some(id);
                        self.message = "Stock location created".into();
                    }
                    Err(e) => self.failure(e),
                }
            }
        });
        ui.separator();
        ui.heading("Stock items");
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
        ui.heading("Edit stock item");
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
                ui.label(format!("Base unit: {} (cannot be changed)", item.unit));
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
        ui.heading("Quantity adjustment");
        ui.label("Adjustments change quantities only; create any accounting entry separately.");
        pick_stock(
            ui,
            "adjustment-item",
            "Item",
            &items,
            &mut self.adjustment_item,
        );
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
                self.store.adjust_stock_at(
                    company,
                    item,
                    self.adjustment_location.ok_or_else(|| {
                        balance::Error::Validation("Select a stock location".into())
                    })?,
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
        ui.heading("Transfer stock");
        ui.label("Transfers change location quantities only and do not create accounting entries. Negative location balances are allowed.");
        pick_stock(ui, "transfer-item", "Item", &items, &mut self.transfer_item);
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
        if ui.button("Save transfer").clicked() {
            let result = (|| -> balance::Result<i64> {
                self.store.transfer_stock(
                    company,
                    self.transfer_item
                        .ok_or_else(|| balance::Error::Validation("Select a stock item".into()))?,
                    self.transfer_from.ok_or_else(|| {
                        balance::Error::Validation("Select a source location".into())
                    })?,
                    self.transfer_to.ok_or_else(|| {
                        balance::Error::Validation("Select a destination location".into())
                    })?,
                    &self.transfer_date,
                    parse_quantity(&self.transfer_quantity)?,
                    &self.transfer_reason,
                )
            })();
            match result {
                Ok(_) => {
                    self.transfer_quantity.clear();
                    self.transfer_reason.clear();
                    self.message = "Stock transfer saved".into();
                }
                Err(e) => self.failure(e),
            }
        }
        egui::CollapsingHeader::new("Stock transfer history").show(ui, |ui| {
            match self.store.stock_transfers(company) {
                Ok(transfers) => {
                    for transfer in transfers {
                        ui.label(format!(
                            "{}: {} {} {} from {} to {} — {}",
                            transfer.date,
                            transfer.item,
                            format_quantity(i128::from(transfer.quantity)),
                            transfer.unit,
                            transfer.from,
                            transfer.to,
                            transfer.reason
                        ));
                    }
                }
                Err(e) => {
                    ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                }
            }
        });
        ui.separator();
        ui.heading("Stock quantities");
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
        let balances = match self.stock_balance_location {
            Some(location) => self.store.stock_location_balances(company, location, end),
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
            }
        }
        ui.small(
            "Quantities may be negative. Stock valuation and cost of goods are not calculated.",
        );
        ui.separator();
        ui.heading("Stock activity");
        ui.small("The location filter above also applies to stock activity; transfers are shown when a location is selected.");
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
            match self
                .store
                .stock_register_at(company, item, self.stock_balance_location, from, to)
            {
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
                    ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                }
            }
        }
    }
    fn invoice_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading(match self.editing_invoice {
            Some(id) => format!("Edit invoice {id}"),
            None => "Sales and purchase invoices".into(),
        });
        ui.label("Save an invoice to create its accounting voucher and stock movements together. Prices are per unit; each line rounds to two decimal places.");
        let items = match self.store.stock_items(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
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
            egui::ComboBox::from_label("Type")
                .selected_text(self.invoice_kind.as_str())
                .show_ui(ui, |ui| {
                    for kind in InvoiceKind::ALL {
                        ui.selectable_value(&mut self.invoice_kind, kind, kind.as_str());
                    }
                });
            ui.label("Date");
            ui.add(egui::TextEdit::singleline(&mut self.invoice_date).hint_text("YYYY-MM-DD"));
            ui.label("Due date (optional)");
            ui.add(egui::TextEdit::singleline(&mut self.invoice_due_date).hint_text("YYYY-MM-DD"));
            ui.label("or credit days");
            ui.add(
                egui::TextEdit::singleline(&mut self.invoice_credit_days)
                    .hint_text("Days")
                    .desired_width(60.0),
            );
            ui.label("Reference");
            ui.text_edit_singleline(&mut self.invoice_reference);
        });
        pick_account(
            ui,
            "invoice-party",
            "Party",
            &ledgers,
            &mut self.invoice_party,
            false,
        );
        pick_account(
            ui,
            "invoice-trade",
            "Sales / purchase ledger",
            &ledgers,
            &mut self.invoice_trade,
            false,
        );
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
        ui.small("Tax is entered manually; statutory rules and filing are not implemented.");
        let mut remove = None;
        for (index, line) in self.invoice_lines.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{}.", index + 1));
                pick_stock(ui, ("invoice-item", index), "Item", &items, &mut line.item);
                pick_location(
                    ui,
                    ("invoice-location", index),
                    "Location",
                    &locations,
                    &mut line.location,
                    false,
                );
                ui.add(
                    egui::TextEdit::singleline(&mut line.quantity)
                        .hint_text("Quantity")
                        .desired_width(90.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut line.price)
                        .hint_text("Unit price")
                        .desired_width(90.0),
                );
                if let (Ok(qty), Ok(price)) =
                    (parse_quantity(&line.quantity), parse_amount(&line.price))
                {
                    if let Ok(amount) = line_amount(qty, price) {
                        ui.label(format!("Amount {}", format_amount(amount)));
                    }
                }
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            if self.invoice_lines.len() > 1 {
                self.invoice_lines.remove(index);
            } else {
                self.message = "An invoice needs at least one line".into();
            }
        }
        if ui.button("Add item line").clicked() {
            self.invoice_lines.push(InvoiceLineForm::new());
        }
        ui.heading("Additional charges and deductions");
        ui.small("Enter each amount manually. These ledger adjustments do not calculate or classify tax.");
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
        if ui.button("Add charge or deduction").clicked() {
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
        if self.editing_invoice.is_some() && ui.button("Cancel invoice edit").clicked() {
            self.editing_invoice = None;
            self.invoice_reference.clear();
            self.invoice_narration.clear();
            self.invoice_due_date.clear();
            self.invoice_credit_days.clear();
            self.invoice_lines = vec![InvoiceLineForm::new()];
            self.invoice_adjustments.clear();
            self.invoice_tax_amount.clear();
            self.invoice_tax_ledger = None;
        }
        if ui.button("Save invoice").clicked() {
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
                        quantity: parse_quantity(&line.quantity)?,
                        unit_price: parse_amount(&line.price)?,
                    });
                }
                let tax_amount = if self.invoice_tax_amount.trim().is_empty() {
                    0
                } else {
                    parse_amount(&self.invoice_tax_amount)?
                };
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
                let draft = InvoiceDraft {
                    kind: self.invoice_kind,
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
                    narration: self.invoice_narration.clone(),
                    party_ledger_id: self.invoice_party.ok_or_else(|| {
                        balance::Error::Validation("Select a party ledger".into())
                    })?,
                    trade_ledger_id: self.invoice_trade.ok_or_else(|| {
                        balance::Error::Validation("Select a sales or purchase ledger".into())
                    })?,
                    tax_ledger_id: self.invoice_tax_ledger,
                    tax_amount,
                    lines,
                    adjustments,
                };
                if let Some(id) = self.editing_invoice {
                    self.store.update_invoice(company, id, &draft)?;
                    Ok(id)
                } else {
                    self.store.create_invoice(company, &draft)
                }
            })();
            match result {
                Ok(id) => {
                    self.message = format!("Invoice {id} saved");
                    self.invoice_reference.clear();
                    self.invoice_narration.clear();
                    self.invoice_due_date.clear();
                    self.invoice_credit_days.clear();
                    self.invoice_lines = vec![InvoiceLineForm::new()];
                    self.invoice_adjustments.clear();
                    self.invoice_tax_amount.clear();
                    self.invoice_tax_ledger = None;
                    self.editing_invoice = None;
                }
                Err(e) => self.failure(e),
            }
        }
        ui.separator();
        ui.heading("Invoices");
        match self.store.invoices(company) {
            Ok(invoices) => {
                ui.heading("Record a receipt or payment");
                ui.small("Allocate one receipt or payment to one or more outstanding invoices of the same party and type.");
                let selected = invoices
                    .iter()
                    .find(|invoice| Some(invoice.id) == self.settlement_invoice)
                    .map(|invoice| {
                        format!(
                            "{} {} — outstanding {}",
                            invoice.kind.as_str(),
                            invoice.reference,
                            money(invoice.outstanding)
                        )
                    })
                    .unwrap_or_else(|| "Select invoice".into());
                egui::ComboBox::from_label("Invoice")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for invoice in &invoices {
                            if invoice.outstanding > 0 {
                                ui.selectable_value(
                                    &mut self.settlement_invoice,
                                    Some(invoice.id),
                                    format!(
                                        "{} {} — {} ({})",
                                        invoice.kind.as_str(),
                                        invoice.reference,
                                        invoice.party,
                                        money(invoice.outstanding)
                                    ),
                                );
                            }
                        }
                    });
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
                        let selected = invoices
                            .iter()
                            .find(|invoice| Some(invoice.id) == line.invoice)
                            .map(|invoice| {
                                format!(
                                    "{} {} — {} ({})",
                                    invoice.kind.as_str(),
                                    invoice.reference,
                                    invoice.party,
                                    money(invoice.outstanding)
                                )
                            })
                            .unwrap_or_else(|| "Select invoice".into());
                        egui::ComboBox::from_id_salt(("settlement-extra", index))
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for invoice in &invoices {
                                    if invoice.outstanding > 0 {
                                        ui.selectable_value(
                                            &mut line.invoice,
                                            Some(invoice.id),
                                            format!(
                                                "{} {} — {} ({})",
                                                invoice.kind.as_str(),
                                                invoice.reference,
                                                invoice.party,
                                                money(invoice.outstanding)
                                            ),
                                        );
                                    }
                                }
                            });
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
                if ui.button("Add invoice allocation").clicked() {
                    self.settlement_extra.push(SettlementLineForm {
                        invoice: None,
                        amount: String::new(),
                    });
                }
                if ui.button("Save receipt / payment").clicked() {
                    let result = (|| -> balance::Result<i64> {
                        let mut allocations = vec![SettlementAllocation {
                            invoice_id: self.settlement_invoice.ok_or_else(|| {
                                balance::Error::Validation("Select an invoice".into())
                            })?,
                            amount: parse_amount(&self.settlement_amount)?,
                        }];
                        for line in &self.settlement_extra {
                            allocations.push(SettlementAllocation {
                                invoice_id: line.invoice.ok_or_else(|| {
                                    balance::Error::Validation(
                                        "Select an invoice for every allocation".into(),
                                    )
                                })?,
                                amount: parse_amount(&line.amount)?,
                            });
                        }
                        let (voucher, _) = self.store.settle_invoices(
                            company,
                            self.settlement_bank.ok_or_else(|| {
                                balance::Error::Validation("Select a cash or bank ledger".into())
                            })?,
                            &self.settlement_date,
                            &self.settlement_reference,
                            &allocations,
                        )?;
                        Ok(voucher)
                    })();
                    match result {
                        Ok(id) => {
                            self.message = format!("Receipt / payment voucher {id} saved");
                            self.settlement_invoice = None;
                            self.settlement_reference.clear();
                            self.settlement_amount.clear();
                            self.settlement_extra.clear();
                        }
                        Err(e) => self.failure(e),
                    }
                }
                ui.separator();
                for invoice in invoices {
                    ui.strong(format!(
                        "{}  {}  {}  {}  Total {}  Paid {}  Outstanding {}",
                        invoice.date,
                        invoice.kind.as_str(),
                        invoice.reference,
                        invoice.party,
                        money(invoice.total),
                        money(invoice.settled),
                        money(invoice.outstanding)
                    ));
                    if ui
                        .add_enabled(
                            invoice.settled == 0 && self.editing_invoice.is_none(),
                            egui::Button::new("Edit invoice"),
                        )
                        .clicked()
                    {
                        match self.store.invoice_draft(company, invoice.id) {
                            Ok(draft) => {
                                self.editing_invoice = Some(invoice.id);
                                self.invoice_kind = draft.kind;
                                self.invoice_date = draft.date;
                                self.invoice_due_date = draft.due_date.unwrap_or_default();
                                self.invoice_credit_days = draft
                                    .credit_days
                                    .map(|days| days.to_string())
                                    .unwrap_or_default();
                                self.invoice_reference = draft.reference;
                                self.invoice_narration = draft.narration;
                                self.invoice_party = Some(draft.party_ledger_id);
                                self.invoice_trade = Some(draft.trade_ledger_id);
                                self.invoice_tax_ledger = draft.tax_ledger_id;
                                self.invoice_tax_amount = if draft.tax_amount == 0 {
                                    String::new()
                                } else {
                                    format_amount(draft.tax_amount)
                                };
                                self.invoice_lines = draft
                                    .lines
                                    .into_iter()
                                    .map(|line| InvoiceLineForm {
                                        item: Some(line.item_id),
                                        location: Some(line.location_id),
                                        quantity: format_quantity(i128::from(line.quantity)),
                                        price: format_amount(line.unit_price),
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
                                self.message = "Invoice loaded for editing".into();
                            }
                            Err(e) => self.failure(e),
                        }
                    }
                    if invoice.settled != 0 {
                        ui.small("Invoices with receipts or payments cannot yet be edited.");
                    }
                    if let Some(due_date) = &invoice.due_date {
                        ui.small(format!("Due: {due_date}"));
                    }
                    if let Some(days) = invoice.credit_days {
                        ui.small(format!("Credit period: {days} days from invoice date"));
                    }
                    for line in invoice.lines {
                        ui.label(format!(
                            "  {} at {}: {} {} × {} = {}",
                            line.item,
                            line.location,
                            format_quantity(i128::from(line.quantity)),
                            line.unit,
                            format_amount(line.unit_price),
                            format_amount(line.amount)
                        ));
                    }
                    if invoice.tax_amount > 0 {
                        ui.label(format!("  Tax: {}", format_amount(invoice.tax_amount)));
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
                                ui.label(format!(
                                    "  {} — {} — {} — {} (voucher #{})",
                                    payment.date,
                                    payment.reference,
                                    payment.bank_ledger,
                                    format_amount(payment.amount),
                                    payment.voucher_id
                                ));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
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
                                            revision.kind.as_str(),
                                            revision.reference,
                                            revision.party
                                        ));
                                        ui.label(format!(
                                            "Ledger: {} • {} • Tax: {}",
                                            revision.trade,
                                            revision.tax_ledger.unwrap_or_default(),
                                            format_amount(revision.tax_amount)
                                        ));
                                        if let Some(due_date) = &revision.due_date {
                                            ui.label(format!("Due: {due_date}"));
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
                                                "  {} at {}: {} {} × {} = {}",
                                                line.item,
                                                line.location,
                                                format_quantity(i128::from(line.quantity)),
                                                line.unit,
                                                format_amount(line.unit_price),
                                                format_amount(line.amount)
                                            ));
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
                                    ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                                }
                            }
                        });
                }
            }
            Err(e) => self.failure(e),
        }
    }
    fn import_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Import accounting XML");
        ui.label("Preview a file before importing. Only supported, complete records can be imported; no changes are made if any issue is found.");
        ui.horizontal(|ui| {
            ui.label("File path");
            if ui
                .add(egui::TextEdit::singleline(&mut self.import_path).desired_width(420.0))
                .changed()
            {
                self.import_preview = None;
            }
        });
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
        if ui.button("Preview file").clicked() {
            self.import_preview = None;
            let result = (|| -> balance::Result<ImportPlan> {
                let path = std::path::Path::new(self.import_path.trim());
                let metadata = std::fs::metadata(path)
                    .map_err(|e| balance::Error::Validation(format!("Cannot read file: {e}")))?;
                if !metadata.is_file() || metadata.len() > 50 * 1024 * 1024 {
                    return Err(balance::Error::Validation(
                        "Choose a file no larger than 50 MiB".into(),
                    ));
                }
                use std::io::Read;
                let file = std::fs::File::open(path)
                    .map_err(|e| balance::Error::Validation(format!("Cannot read file: {e}")))?;
                let mut bytes = Vec::new();
                file.take(50 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| balance::Error::Validation(format!("Cannot read file: {e}")))?;
                self.store.preview_xml_import_with_opening_date(
                    company,
                    &bytes,
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
                "Preview: {} account groups, {} ledgers, {} opening balances, {} vouchers, {} invoices; {} units, {} stock groups, {} locations, {} items, {} opening quantities",
                plan.group_count(),
                plan.ledger_count(),
                plan.opening_count(),
                plan.voucher_count(), plan.invoice_count()
                , plan.unit_count(), plan.stock_group_count(), plan.location_count(), plan.item_count(), plan.stock_opening_count()
            ));
            if plan.issues().is_empty() {
                ui.label("Preview found no issues. Review the limitations below before importing.");
            } else {
                ui.colored_label(
                    Color32::from_rgb(155, 55, 35),
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
                            "Imported {} account groups, {} ledgers, {} vouchers, {} invoices, {} units, {} stock groups, {} locations and {} items",
                            counts.groups, counts.ledgers, counts.vouchers, counts.invoices, counts.units, counts.stock_groups, counts.locations, counts.items
                        );
                        self.import_preview = None;
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
        ui.small("This initial importer handles simple groups, ledgers, selected contact details, opening balances, accounting vouchers, simple units, stock groups, flat stock locations, unvalued stock items and a restricted form of item invoice. Invoices require one matching new bill reference and one trade ledger; additional ledger postings become unclassified charges or deductions. Only one location allocation per item is supported. Tax details, named batches, nested locations, other bill allocations, valued opening stock and other unsupported fields block import. Import has not been verified against representative exports.");
    }
    fn bank_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Bank clearance");
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                return;
            }
        };
        ui.label(format!(
            "Book balance: {}    Cleared postings: {}    Uncleared difference: {}",
            money(report.book_balance),
            money(report.cleared_balance),
            money(report.uncleared)
        ));
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
        self.bank_statement_page(ui, company, ledger, selected_posting);
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
                        ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
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
    ) {
        ui.separator();
        ui.heading("Statement entries");
        ui.small("CSV columns: id,date,description,amount. Dates use YYYY-MM-DD; deposits are positive and withdrawals negative. IDs must be unique for this ledger. Matching requires the exact amount and a statement date on or after the voucher date.");
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), issue);
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
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
                    ui.end_row();
                }
            });
        let Some(entry) = entries
            .iter()
            .find(|entry| Some(entry.id) == self.bank_statement_selection)
        else {
            return;
        };
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
                        ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
                    }
                }
            });
    }
    fn outstanding_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Outstanding invoices");
        ui.label("Age is measured from the invoice date; overdue days are measured from the optional due date. Credit notes and unallocated payments are not included. Past reports use the invoice's current details and due date.");
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
        match self
            .store
            .outstanding_invoices(company, &self.outstanding_date)
        {
            Ok(rows) => {
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
                        "Reference",
                        "Total",
                        "Settled",
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
                        ui.label(row.kind.as_str());
                        ui.label(row.party);
                        ui.label(row.date);
                        ui.label(row.due_date.as_deref().unwrap_or("—"));
                        ui.label(row.reference);
                        ui.label(money(row.total));
                        ui.label(money(row.settled));
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
                ui.heading("Age from invoice date");
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
                ui.heading("Age from due date");
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
            }
        }
    }
    fn day_book_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Day book");
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
                ui.colored_label(Color32::from_rgb(155, 55, 35), e.to_string());
            }
        }
    }
    fn backup_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Back up data");
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
        ui.heading("Restore to a new file");
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
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Balance").color(Color32::from_rgb(30, 75, 115)));
                ui.label("Early accounting prototype");
            });
            ui.small(format!("Database: {}", self.database_path.display()));
        });
        egui::SidePanel::left("navigation").show(ctx, |ui| {
            ui.heading("Gateway");
            for (label, page) in [
                ("Companies", Page::Company),
                ("Ledgers", Page::Ledger),
                ("Voucher", Page::Voucher),
                ("Ledger activity", Page::Register),
                ("Day book", Page::DayBook),
                ("Outstanding", Page::Outstanding),
                ("Bank clearance", Page::Bank),
                ("Stock", Page::Stock),
                ("Invoices", Page::Invoice),
                ("Import XML", Page::Import),
                ("Back up", Page::Backup),
                ("Reports", Page::Reports),
            ] {
                if ui.selectable_label(self.page == page, label).clicked() {
                    self.page = page;
                    self.message.clear();
                }
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if !self.message.is_empty() {
                    ui.colored_label(Color32::from_rgb(155, 55, 35), &self.message);
                    ui.separator();
                }
                if self.page == Page::Company {
                    self.company_page(ui);
                    return;
                }
                if self.page == Page::Backup {
                    self.backup_page(ui);
                    return;
                }
                let Some(company) = self.company else {
                    ui.label("Select a company first.");
                    return;
                };
                match self.page {
                    Page::Company => {}
                    Page::Ledger => self.ledger_page(ui, company),
                    Page::Voucher => self.voucher_page(ui, company),
                    Page::Register => self.register_page(ui, company),
                    Page::DayBook => self.day_book_page(ui, company),
                    Page::Outstanding => self.outstanding_page(ui, company),
                    Page::Bank => self.bank_page(ui, company),
                    Page::Stock => self.stock_page(ui, company),
                    Page::Invoice => self.invoice_page(ui, company),
                    Page::Import => self.import_page(ui, company),
                    Page::Backup => {}
                    Page::Reports => self.reports_page(ui, company),
                }
            });
        });
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
    eframe::run_native(
        "Balance",
        eframe::NativeOptions::default(),
        Box::new(move |_| Ok(Box::new(App::new(store, database_path)))),
    )?;
    Ok(())
}
