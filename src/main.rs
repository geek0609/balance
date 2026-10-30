#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, RichText};
use optimus_prime::{format_amount, parse_amount, Category, Posting, Store, VoucherKind};

#[derive(PartialEq)]
enum Page {
    Company,
    Ledger,
    Voucher,
    Register,
    Reports,
}
struct App {
    store: Store,
    page: Page,
    company: Option<i64>,
    company_name: String,
    ledger_name: String,
    category: Category,
    ledger_group: Option<i64>,
    group_name: String,
    group_parent: Option<i64>,
    lines: Vec<VoucherLine>,
    editing_voucher: Option<i64>,
    voucher_kind: VoucherKind,
    date: String,
    narration: String,
    report_from: String,
    report_to: String,
    register_ledger: Option<i64>,
    register_from: String,
    register_to: String,
    message: String,
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
    fn new(store: Store) -> Self {
        Self {
            store,
            page: Page::Company,
            company: None,
            company_name: String::new(),
            ledger_name: String::new(),
            category: Category::Asset,
            ledger_group: None,
            group_name: String::new(),
            group_parent: None,
            lines: vec![VoucherLine::new(true), VoucherLine::new(false)],
            editing_voucher: None,
            voucher_kind: VoucherKind::Journal,
            date: String::new(),
            narration: String::new(),
            report_from: String::new(),
            report_to: String::new(),
            register_ledger: None,
            register_from: String::new(),
            register_to: String::new(),
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
        self.narration.clear();
    }
    fn reset_company_forms(&mut self) {
        self.clear_voucher();
        self.register_ledger = None;
        self.ledger_group = None;
        self.group_parent = None;
    }
    fn company_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Companies");
        ui.label("Create or select a company to work with.");
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.company_name);
            if ui.button("Create company").clicked() {
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
                        .selectable_label(self.company == Some(company.id), &company.name)
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
                None => Err(optimus_prime::Error::Validation(
                    "Select an account group".into(),
                )),
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
        match self.store.ledgers(company) {
            Ok(ledgers) => {
                for ledger in ledgers {
                    let group = groups
                        .iter()
                        .find(|g| Some(g.id) == ledger.group_id)
                        .map(|g| g.name.as_str())
                        .unwrap_or(ledger.category.as_str());
                    ui.label(format!("{}  —  {}", ledger.name, group));
                }
            }
            Err(e) => self.failure(e),
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
            let result = (|| -> optimus_prime::Result<i64> {
                let mut postings = Vec::with_capacity(self.lines.len());
                for line in &self.lines {
                    let ledger_id = line.ledger.ok_or_else(|| {
                        optimus_prime::Error::Validation("Select a ledger for each line".into())
                    })?;
                    let amount = parse_amount(&line.amount)?;
                    if amount == 0 {
                        return Err(optimus_prime::Error::Validation(
                            "Amounts must be greater than zero".into(),
                        ));
                    }
                    postings.push(Posting {
                        ledger_id,
                        amount: if line.debit { amount } else { -amount },
                    });
                }
                if let Some(id) = self.editing_voucher {
                    self.store.update_typed_voucher(
                        company,
                        id,
                        self.voucher_kind,
                        &self.date,
                        &self.narration,
                        &postings,
                    )?;
                    Ok(id)
                } else {
                    self.store.post_typed_voucher(
                        company,
                        self.voucher_kind,
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
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{}  #{}  {}",
                            voucher.date, voucher.id, voucher.narration
                        ));
                        ui.label(voucher.kind.as_str());
                        if ui.button("Edit").clicked() {
                            self.editing_voucher = Some(voucher.id);
                            self.voucher_kind = voucher.kind;
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
                                            "Replaced at {} UTC • {} • {} • {}",
                                            revision.changed_at,
                                            revision.voucher.date,
                                            revision.voucher.kind.as_str(),
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
                ui.heading(RichText::new("Optimus Prime").color(Color32::from_rgb(30, 75, 115)));
                ui.label("Early accounting prototype");
            });
        });
        egui::SidePanel::left("navigation").show(ctx, |ui| {
            ui.heading("Gateway");
            for (label, page) in [
                ("Companies", Page::Company),
                ("Ledgers", Page::Ledger),
                ("Voucher", Page::Voucher),
                ("Ledger activity", Page::Register),
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
                let Some(company) = self.company else {
                    ui.label("Select a company first.");
                    return;
                };
                match self.page {
                    Page::Company => {}
                    Page::Ledger => self.ledger_page(ui, company),
                    Page::Voucher => self.voucher_page(ui, company),
                    Page::Register => self.register_page(ui, company),
                    Page::Reports => self.reports_page(ui, company),
                }
            });
        });
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dirs = directories::ProjectDirs::from("org", "Optimus Prime", "Optimus Prime")
        .ok_or("Cannot find application data directory")?;
    std::fs::create_dir_all(dirs.data_local_dir())?;
    let store = Store::open(dirs.data_local_dir().join("optimus-prime.db"))?;
    eframe::run_native(
        "Optimus Prime",
        eframe::NativeOptions::default(),
        Box::new(move |_| Ok(Box::new(App::new(store)))),
    )?;
    Ok(())
}
