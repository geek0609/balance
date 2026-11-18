use super::{pick_account, App};
use balance::currencies::{
    format_exchange_rate, format_foreign_amount, parse_exchange_rate, parse_foreign_amount,
    ForeignCurrency, ForeignPosting, ForeignVoucherDraft, ForeignVoucherLine, ForeignVoucherPlan,
};
use balance::{format_amount, Category, Error, Result, VoucherKind};
use eframe::egui;

const FX_KINDS: [VoucherKind; 4] = [
    VoucherKind::Journal,
    VoucherKind::Payment,
    VoucherKind::Receipt,
    VoucherKind::Transfer,
];

struct ForeignLineForm {
    ledger: Option<i64>,
    debit: bool,
    amount: String,
}
impl ForeignLineForm {
    fn new(debit: bool) -> Self {
        Self {
            ledger: None,
            debit,
            amount: String::new(),
        }
    }
}

struct ForeignForm {
    currency: Option<i64>,
    kind: VoucherKind,
    date: String,
    reference: String,
    narration: String,
    rounding: Option<i64>,
    rows: Vec<ForeignLineForm>,
}
impl Default for ForeignForm {
    fn default() -> Self {
        Self {
            currency: None,
            kind: VoucherKind::Journal,
            date: String::new(),
            reference: String::new(),
            narration: String::new(),
            rounding: None,
            rows: vec![ForeignLineForm::new(true), ForeignLineForm::new(false)],
        }
    }
}
impl ForeignForm {
    fn draft(&self, currencies: &[ForeignCurrency]) -> Result<ForeignVoucherDraft> {
        let chosen = currencies
            .iter()
            .find(|row| Some(row.id) == self.currency)
            .ok_or_else(|| Error::Validation("Select a foreign currency".into()))?;
        let postings = self
            .rows
            .iter()
            .map(|entry| {
                let amount = parse_foreign_amount(&entry.amount, chosen.decimal_places)?;
                Ok(ForeignPosting {
                    ledger_id: entry.ledger.ok_or_else(|| {
                        Error::Validation("Select a ledger for every line".into())
                    })?,
                    amount: if entry.debit { amount } else { -amount },
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ForeignVoucherDraft {
            currency_id: chosen.id,
            kind: self.kind,
            date: self.date.clone(),
            reference: self.reference.clone(),
            narration: self.narration.clone(),
            rounding_ledger: self.rounding,
            postings,
        })
    }
}

#[derive(Default)]
pub(super) struct CurrencyScreen {
    base: String,
    code: String,
    places: u8,
    rate_currency: Option<i64>,
    rate_date: String,
    rate: String,
    rate_reason: String,
    draft: ForeignForm,
    preview: Option<ForeignVoucherPlan>,
    register_currency: Option<i64>,
    search: String,
    selected: Option<i64>,
}
impl CurrencyScreen {
    pub(super) fn has_preview(&self) -> bool {
        self.preview.is_some()
    }
}

impl App {
    pub(super) fn currency_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Currencies");
        let base = match self.store.company_base_currency(company) {
            Ok(base) => base,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let currencies = match self.store.foreign_currencies(company) {
            Ok(currencies) => currencies,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        egui::CollapsingHeader::new("Currency settings")
            .default_open(base.is_none())
            .show(ui, |ui| {
                self.currency_settings(ui, company, base.as_deref(), &currencies);
            });
        if let Some(base) = &base {
            ui.label(format!("Ledger amounts use {base}."));
        }
        egui::CollapsingHeader::new("Exchange rates").show(ui, |ui| {
            self.exchange_rate_form(ui, company, &currencies, base.as_deref().unwrap_or("Base"));
        });
        egui::CollapsingHeader::new("Foreign voucher")
            .default_open(true)
            .show(ui, |ui| {
                if self.currency_screen.has_preview() {
                    self.foreign_preview(ui, company);
                } else {
                    self.foreign_form(ui, company, &currencies);
                }
            });
        egui::CollapsingHeader::new("Foreign voucher register").show(ui, |ui| {
            self.foreign_register(ui, company, &currencies);
        });
    }

    fn currency_settings(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        base: Option<&str>,
        currencies: &[ForeignCurrency],
    ) {
        if let Some(base) = base {
            ui.label(format!("Base currency {base}"));
        }
        if currencies.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.currency_screen.base)
                        .hint_text("Base currency code")
                        .desired_width(135.0),
                );
                if ui.button("Set base currency").clicked() {
                    match self
                        .store
                        .set_company_base_currency(company, &self.currency_screen.base)
                    {
                        Ok(()) => {
                            self.currency_screen.base.clear();
                            self.message = "Base currency saved".into();
                        }
                        Err(error) => self.failure(error),
                    }
                }
            });
            ui.small("Set the code for the amounts already in the company books. The base code is fixed after a foreign currency is added.");
        }
        if base.is_none() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.currency_screen.code)
                    .hint_text("Foreign currency code")
                    .desired_width(145.0),
            );
            ui.label("Decimal places");
            ui.add(egui::DragValue::new(&mut self.currency_screen.places).range(0..=6));
            if ui.button("Add currency").clicked() {
                match self.store.add_foreign_currency(
                    company,
                    &self.currency_screen.code,
                    self.currency_screen.places,
                ) {
                    Ok(id) => {
                        self.currency_screen.code.clear();
                        self.currency_screen.rate_currency = Some(id);
                        self.message = "Foreign currency added".into();
                    }
                    Err(error) => self.failure(error),
                }
            }
        });
        for currency in currencies {
            ui.label(format!(
                "{} | {} decimal places",
                currency.code, currency.decimal_places
            ));
        }
    }

    fn exchange_rate_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        currencies: &[ForeignCurrency],
        base: &str,
    ) {
        if currencies.is_empty() {
            ui.label("Add a foreign currency to record its rates.");
            return;
        }
        let form = &mut self.currency_screen;
        pick_currency(
            ui,
            "rate-currency",
            currencies,
            &mut form.rate_currency,
            false,
        );
        let Some(selected) = currencies
            .iter()
            .find(|row| Some(row.id) == form.rate_currency)
        else {
            return;
        };
        ui.small(format!("Enter {} per one {}.", base, selected.code));
        egui::Grid::new("exchange-rate-fields").show(ui, |ui| {
            for (label, value) in [
                ("Effective date (YYYY-MM-DD)", &mut form.rate_date),
                ("Rate", &mut form.rate),
                ("Reason", &mut form.rate_reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        if ui.button("Record exchange rate").clicked() {
            match parse_exchange_rate(&form.rate).and_then(|rate| {
                self.store.record_foreign_rate(
                    company,
                    selected.id,
                    &form.rate_date,
                    rate,
                    &form.rate_reason,
                )
            }) {
                Ok(_) => {
                    form.rate.clear();
                    form.rate_reason.clear();
                    self.message = "Exchange rate recorded".into();
                }
                Err(error) => self.message = error.to_string(),
            }
        }
        egui::CollapsingHeader::new("Exchange rate history").show(ui, |ui| {
            match self.store.foreign_rates(company, selected.id) {
                Ok(rates) => {
                    for rate in rates {
                        ui.label(format!(
                            "Effective {} | {} | {} | {} UTC",
                            rate.date,
                            format_exchange_rate(rate.rate),
                            rate.reason,
                            rate.recorded_at
                        ));
                    }
                }
                Err(error) => self.failure(error),
            }
        });
    }

    fn foreign_form(&mut self, ui: &mut egui::Ui, company: i64, currencies: &[ForeignCurrency]) {
        if currencies.is_empty() {
            ui.label("Set the base currency and add a foreign currency and rate.");
            return;
        }
        let ledgers = match self.store.ledgers(company) {
            Ok(ledgers) => ledgers,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let rounding = ledgers
            .iter()
            .filter(|ledger| matches!(ledger.category, Category::Income | Category::Expense))
            .cloned()
            .collect::<Vec<_>>();
        let draft = &mut self.currency_screen.draft;
        ui.horizontal_wrapped(|ui| {
            pick_currency(
                ui,
                "fx-draft-currency",
                currencies,
                &mut draft.currency,
                false,
            );
            egui::ComboBox::from_id_salt("fx-draft-kind")
                .selected_text(draft.kind.as_str())
                .show_ui(ui, |ui| {
                    for kind in FX_KINDS {
                        ui.selectable_value(&mut draft.kind, kind, kind.as_str());
                    }
                });
        });
        egui::Grid::new("foreign-voucher-fields").show(ui, |ui| {
            for (label, value) in [
                ("Date (YYYY-MM-DD)", &mut draft.date),
                ("Reference", &mut draft.reference),
                ("Narration", &mut draft.narration),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        pick_account(
            ui,
            "fx-rounding-ledger",
            "Rounding ledger (optional)",
            &rounding,
            &mut draft.rounding,
            true,
        );
        let mut remove = None;
        for (index, line) in draft.rows.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                pick_account(
                    ui,
                    ("fx-ledger", index),
                    "Ledger",
                    &ledgers,
                    &mut line.ledger,
                    false,
                );
                ui.selectable_value(&mut line.debit, true, "Debit");
                ui.selectable_value(&mut line.debit, false, "Credit");
                ui.add(
                    egui::TextEdit::singleline(&mut line.amount)
                        .hint_text("Foreign amount")
                        .desired_width(100.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            draft.rows.remove(index);
        }
        if ui.button("Add posting").clicked() {
            draft.rows.push(ForeignLineForm::new(true));
        }
        if ui.button("Preview conversion").clicked() {
            match draft
                .draft(currencies)
                .and_then(|draft| self.store.preview_foreign_voucher(company, &draft))
            {
                Ok(preview) => {
                    self.currency_screen.preview = Some(preview);
                    self.message.clear();
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn foreign_preview(&mut self, ui: &mut egui::Ui, company: i64) {
        let Some(plan) = self.currency_screen.preview.as_ref() else {
            return;
        };
        ui.label(format!(
            "{} | {} {} per {} | Rate effective {}",
            plan.date(),
            format_exchange_rate(plan.rate().rate),
            plan.base_code(),
            plan.currency().code,
            plan.rate().date
        ));
        show_lines(
            ui,
            "foreign-preview",
            plan.currency(),
            plan.base_code(),
            plan.lines(),
        );
        let mut edit = false;
        let mut post = false;
        ui.horizontal_wrapped(|ui| {
            edit = ui.button("Edit voucher").clicked();
            post = ui.button("Post foreign voucher").clicked();
        });
        if edit {
            self.currency_screen.preview = None;
        } else if post {
            match self.store.post_foreign_voucher(company, plan) {
                Ok(voucher) => {
                    self.currency_screen.draft = ForeignForm::default();
                    self.currency_screen.preview = None;
                    self.currency_screen.selected = Some(voucher);
                    self.message = format!("Foreign voucher {voucher} posted");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn foreign_register(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        currencies: &[ForeignCurrency],
    ) {
        pick_currency(
            ui,
            "fx-register-currency",
            currencies,
            &mut self.currency_screen.register_currency,
            true,
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.currency_screen.search)
                .hint_text("Reference or voucher ID"),
        );
        let search = self.currency_screen.search.trim().to_lowercase();
        match self.store.foreign_vouchers(company) {
            Ok(vouchers) => {
                let mut found = false;
                for voucher in vouchers {
                    if self
                        .currency_screen
                        .register_currency
                        .is_some_and(|id| id != voucher.currency.id)
                        || (!search.is_empty()
                            && !voucher.reference.to_lowercase().contains(&search)
                            && voucher.id.to_string() != search)
                    {
                        continue;
                    }
                    found = true;
                    let selected = self.currency_screen.selected == Some(voucher.id);
                    let label = format!(
                        "{} | {} #{} | {} | {}",
                        voucher.date,
                        voucher.kind.as_str(),
                        voucher.id,
                        voucher.reference,
                        voucher.currency.code
                    );
                    if ui.selectable_label(selected, label).clicked() {
                        self.currency_screen.selected = (!selected).then_some(voucher.id);
                    }
                    if self.currency_screen.selected == Some(voucher.id) {
                        ui.label(format!(
                            "{} {} per {} | Rate effective {}",
                            format_exchange_rate(voucher.rate.rate),
                            voucher.base_code,
                            voucher.currency.code,
                            voucher.rate.date
                        ));
                        match self.store.foreign_voucher_lines(company, voucher.id) {
                            Ok(lines) => show_lines(
                                ui,
                                ("foreign-register", voucher.id),
                                &voucher.currency,
                                &voucher.base_code,
                                &lines,
                            ),
                            Err(error) => self.failure(error),
                        }
                        if let Ok(Some(reversal)) = self.store.voucher_reversal(company, voucher.id)
                        {
                            ui.small(format!(
                                "Original voucher {} | Reversal voucher {} on {}",
                                reversal.original_voucher_id,
                                reversal.reversal_voucher_id,
                                reversal.date
                            ));
                        }
                    }
                }
                if !found {
                    ui.label("No matching foreign vouchers.");
                }
            }
            Err(error) => self.failure(error),
        }
    }
}

fn pick_currency(
    ui: &mut egui::Ui,
    id: &str,
    rows: &[ForeignCurrency],
    value: &mut Option<i64>,
    all: bool,
) {
    let prompt = if all {
        "All currencies"
    } else {
        "Select currency"
    };
    let label = rows
        .iter()
        .find(|row| Some(row.id) == *value)
        .map(|row| row.code.as_str())
        .unwrap_or(prompt);
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .show_ui(ui, |ui| {
            if all {
                ui.selectable_value(value, None, prompt);
            }
            for row in rows {
                ui.selectable_value(value, Some(row.id), &row.code);
            }
        });
}

fn show_lines(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    currency: &ForeignCurrency,
    base: &str,
    lines: &[ForeignVoucherLine],
) {
    egui::Grid::new(id).striped(true).show(ui, |ui| {
        ui.strong("Ledger");
        ui.strong(&currency.code);
        ui.strong(base);
        ui.end_row();
        for line in lines {
            ui.label(&line.ledger);
            ui.label(
                line.foreign_amount
                    .map(|amount| format_foreign_amount(amount, currency.decimal_places))
                    .unwrap_or_else(|| "Rounding".into()),
            );
            ui.label(format_amount(line.base_amount));
            ui.end_row();
        }
    });
    ui.small("Debits are positive and credits are negative.");
}
