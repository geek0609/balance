#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, RichText};
use optimus_prime::{format_amount, parse_amount, Category, Ledger, Posting, Store};

#[derive(PartialEq)]
enum Page {
    Company,
    Ledger,
    Voucher,
    Reports,
}
struct App {
    store: Store,
    page: Page,
    company: Option<i64>,
    company_name: String,
    ledger_name: String,
    category: Category,
    debit: Option<i64>,
    credit: Option<i64>,
    amount: String,
    date: String,
    narration: String,
    message: String,
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
            debit: None,
            credit: None,
            amount: String::new(),
            date: String::new(),
            narration: String::new(),
            message: String::new(),
        }
    }
    fn failure(&mut self, e: impl std::fmt::Display) {
        self.message = e.to_string();
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
                        self.debit = None;
                        self.credit = None;
                        self.message.clear();
                    }
                }
            }
            Err(e) => self.failure(e),
        }
    }
    fn ledger_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Ledgers");
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.ledger_name);
            egui::ComboBox::from_label("Category")
                .selected_text(self.category.as_str())
                .show_ui(ui, |ui| {
                    for c in Category::ALL {
                        ui.selectable_value(&mut self.category, c, c.as_str());
                    }
                });
        });
        if ui.button("Create ledger").clicked() {
            match self
                .store
                .create_ledger(company, &self.ledger_name, self.category)
            {
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
                    ui.label(format!("{}  —  {}", ledger.name, ledger.category.as_str()));
                }
            }
            Err(e) => self.failure(e),
        }
    }
    fn voucher_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Journal voucher");
        ui.label("Enter one debit and one credit. Amounts use two decimal places.");
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
        ledger_picker(ui, "Debit", &ledgers, &mut self.debit);
        ledger_picker(ui, "Credit", &ledgers, &mut self.credit);
        ui.horizontal(|ui| {
            ui.label("Amount");
            ui.text_edit_singleline(&mut self.amount);
        });
        ui.horizontal(|ui| {
            ui.label("Narration");
            ui.text_edit_singleline(&mut self.narration);
        });
        if ui.button("Post voucher").clicked() {
            let result = (|| -> optimus_prime::Result<i64> {
                let debit = self.debit.ok_or_else(|| {
                    optimus_prime::Error::Validation("Select a debit ledger".into())
                })?;
                let credit = self.credit.ok_or_else(|| {
                    optimus_prime::Error::Validation("Select a credit ledger".into())
                })?;
                if debit == credit {
                    return Err(optimus_prime::Error::Validation(
                        "Choose different ledgers".into(),
                    ));
                }
                let amount = parse_amount(&self.amount)?;
                if amount == 0 {
                    return Err(optimus_prime::Error::Validation(
                        "Amount must be greater than zero".into(),
                    ));
                }
                self.store.post_voucher(
                    company,
                    &self.date,
                    &self.narration,
                    &[
                        Posting {
                            ledger_id: debit,
                            amount,
                        },
                        Posting {
                            ledger_id: credit,
                            amount: -amount,
                        },
                    ],
                )
            })();
            match result {
                Ok(id) => {
                    self.message = format!("Voucher {id} posted");
                    self.amount.clear();
                    self.narration.clear();
                }
                Err(e) => self.failure(e),
            }
        }
    }
    fn reports_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Trial balance");
        ui.label("All posted vouchers; debit balances are positive, credits negative.");
        let balances = match self.store.balances(company) {
            Ok(v) => v,
            Err(e) => {
                self.failure(e);
                return;
            }
        };
        let mut totals = [0_i128; 5];
        egui::Grid::new("trial_balance")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Ledger");
                ui.strong("Category");
                ui.strong("Balance");
                ui.end_row();
                for b in &balances {
                    ui.label(&b.ledger.name);
                    ui.label(b.ledger.category.as_str());
                    ui.label(format_amount(b.amount));
                    ui.end_row();
                    let index = Category::ALL
                        .iter()
                        .position(|c| *c == b.ledger.category)
                        .unwrap();
                    totals[index] += i128::from(b.amount);
                }
            });
        ui.separator();
        let income = -totals[3];
        let expense = totals[4];
        let profit = income - expense;
        ui.heading("Profit and loss");
        ui.label(format!(
            "Income: {}    Expenses: {}    Net profit: {}",
            money(income),
            money(expense),
            money(profit)
        ));
        ui.heading("Balance sheet");
        ui.label(format!(
            "Assets: {}    Liabilities: {}    Equity including profit: {}",
            money(totals[0]),
            money(-totals[1]),
            money(-totals[2] + profit)
        ));
        ui.small("Reports cover all dates; opening balances, tax, and statutory adjustments are not implemented.");
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
fn ledger_picker(ui: &mut egui::Ui, label: &str, ledgers: &[Ledger], selected: &mut Option<i64>) {
    let text = ledgers
        .iter()
        .find(|l| Some(l.id) == *selected)
        .map(|l| l.name.as_str())
        .unwrap_or("Select ledger");
    egui::ComboBox::from_label(label)
        .selected_text(text)
        .show_ui(ui, |ui| {
            for ledger in ledgers {
                ui.selectable_value(selected, Some(ledger.id), &ledger.name);
            }
        });
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
                ("Reports", Page::Reports),
            ] {
                if ui.selectable_label(self.page == page, label).clicked() {
                    self.page = page;
                    self.message.clear();
                }
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
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
                Page::Reports => self.reports_page(ui, company),
            }
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
