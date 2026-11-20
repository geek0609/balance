use super::{money, pick_account, App, InvoiceTaxForm};
use balance::inventory::{discounted_line_amount_with_basis, parse_quantity, InvoiceKind};
use balance::tax_profiles::{format_tax_rate, parse_tax_rate, TaxProfileDraft, TaxRate};
use balance::taxes::{format_source_tax_rates, InvoiceTaxRegister};
use balance::{format_amount, parse_amount, Category, Error, Result};
use eframe::egui;

#[derive(Default)]
pub(super) struct TaxRegisterScreen {
    from: String,
    to: String,
    kind: Option<InvoiceKind>,
    ledger: Option<i64>,
    report: Option<InvoiceTaxRegister>,
}

impl App {
    pub(super) fn tax_register_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Invoice tax register");
        ui.small("Recorded invoice tax and full reversals. Dates apply separately to invoices and notes.");
        egui::CollapsingHeader::new("Tax calculation profiles").show(ui, |ui| {
            self.tax_profile_settings(ui, company);
        });
        self.report_export_path_field(ui);
        let ledgers = match self.store.ledgers(company) {
            Ok(rows) => rows
                .into_iter()
                .filter(|row| matches!(row.category, Category::Asset | Category::Liability))
                .collect::<Vec<_>>(),
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let form = &mut self.tax_register_screen;
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            for (label, input) in [("From", &mut form.from), ("Through", &mut form.to)] {
                ui.label(label);
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(input)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(105.0),
                    )
                    .changed();
            }
            let previous = form.kind;
            egui::ComboBox::from_id_salt("tax-invoice-kind")
                .selected_text(form.kind.map(InvoiceKind::as_str).unwrap_or("All types"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.kind, None, "All types");
                    for kind in InvoiceKind::ALL {
                        ui.selectable_value(&mut form.kind, Some(kind), kind.as_str());
                    }
                });
            changed |= form.kind != previous;
        });
        let prev_ledger = form.ledger;
        pick_account(
            ui,
            "tax-register-ledger",
            "Ledger (optional)",
            &ledgers,
            &mut form.ledger,
            true,
        );
        if changed || form.ledger != prev_ledger {
            form.report = None;
        }
        let from = form.from.trim();
        let to = form.to.trim();
        let from = (!from.is_empty()).then_some(from);
        let to = (!to.is_empty()).then_some(to);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Run register").clicked() {
                match self
                    .store
                    .invoice_tax_register(company, from, to, form.kind, form.ledger)
                {
                    Ok(report) => {
                        form.report = Some(report);
                        self.message.clear();
                    }
                    Err(error) => {
                        form.report = None;
                        self.message = error.to_string();
                    }
                }
            }
            if ui.button("Export tax register CSV").clicked() {
                match self.store.export_invoice_tax_register_csv(
                    company,
                    from,
                    to,
                    form.kind,
                    form.ledger,
                    self.report_export_path.trim(),
                ) {
                    Ok(()) => self.message = "Invoice tax register exported".into(),
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
        let Some(report) = &form.report else { return };
        ui.label(format!(
            "Sales tax {} | Purchase tax {}",
            money(report.sales),
            money(report.purchases)
        ));
        if report.entries.is_empty() {
            ui.label("No recorded invoice taxes match these filters.");
        }
        egui::CollapsingHeader::new("Tax by ledger")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("tax-by-ledger")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in ["Tax ledger", "Sales tax", "Purchase tax"] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for row in &report.ledgers {
                            ui.label(&row.ledger);
                            ui.label(money(row.sales));
                            ui.label(money(row.purchases));
                            ui.end_row();
                        }
                    });
            });
        egui::ScrollArea::both()
            .id_salt("invoice-tax-detail")
            .max_height(420.0)
            .show(ui, |ui| {
                egui::Grid::new("invoice-tax-entries")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in [
                            "Date",
                            "Type",
                            "Document",
                            "Reference",
                            "Party",
                            "Tax ledger",
                            "Amount",
                            "Source rates",
                        ] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for row in &report.entries {
                            ui.label(row.date.to_string());
                            ui.label(row.kind.as_str());
                            ui.label(row.document_type());
                            ui.label(&row.reference);
                            ui.label(&row.party);
                            ui.label(&row.ledger);
                            ui.label(money(row.amount));
                            ui.label(format_source_tax_rates(&row.source_rates));
                            ui.end_row();
                        }
                    });
            });
    }
}

#[derive(Default)]
struct TaxRateForm {
    ledger: Option<i64>,
    rate: String,
}

pub(super) struct TaxProfileScreen {
    kind: InvoiceKind,
    name: String,
    date: String,
    reason: String,
    rates: Vec<TaxRateForm>,
}
impl Default for TaxProfileScreen {
    fn default() -> Self {
        Self {
            kind: InvoiceKind::Sale,
            name: String::new(),
            date: String::new(),
            reason: String::new(),
            rates: Vec::new(),
        }
    }
}
impl TaxProfileScreen {
    fn draft(&self) -> Result<TaxProfileDraft> {
        Ok(TaxProfileDraft {
            kind: self.kind,
            name: self.name.clone(),
            effective_on: self.date.clone(),
            reason: self.reason.clone(),
            rates: self
                .rates
                .iter()
                .map(|line| {
                    Ok(TaxRate {
                        ledger_id: line.ledger.ok_or_else(|| {
                            Error::Validation("Select a ledger for each rate".into())
                        })?,
                        rate: parse_tax_rate(&line.rate)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl App {
    pub(super) fn clear_invoice_tax_context(&mut self) {
        self.invoice_tax_calculation = None;
        self.invoice_tax_calculation_date.clear();
        self.invoice_tax_profile = None;
        self.invoice_taxable_base.clear();
    }

    fn tax_profile_settings(&mut self, ui: &mut egui::Ui, company: i64) {
        let ledgers = match self.store.ledgers(company) {
            Ok(ledgers) => ledgers
                .into_iter()
                .filter(|ledger| matches!(ledger.category, Category::Asset | Category::Liability))
                .collect::<Vec<_>>(),
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let form = &mut self.tax_profile_screen;
        ui.small("Saving the same name and invoice type adds a rate version.");
        egui::ComboBox::from_id_salt("tax-profile-kind")
            .selected_text(form.kind.as_str())
            .show_ui(ui, |ui| {
                for kind in InvoiceKind::ALL {
                    ui.selectable_value(&mut form.kind, kind, kind.as_str());
                }
            });
        egui::Grid::new("tax-profile-fields").show(ui, |ui| {
            for (label, value) in [
                ("Profile name", &mut form.name),
                ("Effective date (YYYY-MM-DD)", &mut form.date),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let mut remove = None;
        for (index, line) in form.rates.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                pick_account(
                    ui,
                    ("tax-profile-ledger", index),
                    "Tax ledger",
                    &ledgers,
                    &mut line.ledger,
                    false,
                );
                ui.add(
                    egui::TextEdit::singleline(&mut line.rate)
                        .hint_text("Percent")
                        .desired_width(85.0),
                );
                if ui.button("Remove rate").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            form.rates.remove(index);
        }
        ui.horizontal(|ui| {
            if ui.button("Add rate").clicked() {
                form.rates.push(TaxRateForm::default());
            }
            if ui.button("Save rate version").clicked() {
                match form
                    .draft()
                    .and_then(|draft| self.store.save_tax_profile_version(company, &draft))
                {
                    Ok(version) => {
                        *form = TaxProfileScreen::default();
                        self.message = format!("Tax profile version {version} saved");
                    }
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
        match self.store.tax_profiles(company) {
            Ok(profiles) => {
                for profile in profiles {
                    egui::CollapsingHeader::new(format!(
                        "{} | {}",
                        profile.kind.as_str(),
                        profile.name
                    ))
                    .id_salt(profile.id)
                    .show(ui, |ui| {
                        match self.store.tax_profile_versions(company, profile.id) {
                            Ok(versions) => {
                                for version in versions {
                                    ui.label(format!(
                                        "Version {} | Effective {} | {} UTC | {}",
                                        version.id,
                                        version.effective_on,
                                        version.recorded_at,
                                        version.reason
                                    ));
                                    for rate in &version.rates {
                                        let ledger = ledgers
                                            .iter()
                                            .find(|ledger| ledger.id == rate.ledger_id)
                                            .map(|ledger| ledger.name.as_str())
                                            .unwrap_or("Unknown ledger");
                                        ui.small(format!(
                                            "{ledger} | {}%",
                                            format_tax_rate(rate.rate)
                                        ));
                                    }
                                    if ui
                                        .push_id(version.id, |ui| ui.button("Copy into rate form"))
                                        .inner
                                        .clicked()
                                    {
                                        self.tax_profile_screen = TaxProfileScreen {
                                            kind: profile.kind,
                                            name: profile.name.clone(),
                                            date: version.effective_on,
                                            reason: String::new(),
                                            rates: version
                                                .rates
                                                .into_iter()
                                                .map(|rate| TaxRateForm {
                                                    ledger: Some(rate.ledger_id),
                                                    rate: format_tax_rate(rate.rate),
                                                })
                                                .collect(),
                                        };
                                    }
                                }
                            }
                            Err(error) => self.failure(error),
                        }
                    });
                }
            }
            Err(error) => self.failure(error),
        }
    }

    fn invoice_net_before_tax(&self) -> Result<i64> {
        let mut net = 0_i128;
        for line in &self.invoice_lines {
            let quantity = match super::parse_optional_quantity(&line.billed_quantity)? {
                Some(quantity) => quantity,
                None => parse_quantity(&line.quantity)?,
            };
            let price = line.price_amount()?;
            net += i128::from(
                discounted_line_amount_with_basis(
                    quantity,
                    price,
                    &line.discount()?,
                    line.rate_basis.as_ref(),
                )?
                .0,
            );
        }
        for line in &self.invoice_adjustments {
            let amount = i128::from(parse_amount(&line.amount)?);
            net += if line.increase { amount } else { -amount };
        }
        let net = i64::try_from(net)
            .map_err(|_| Error::Validation("Invoice amount before tax is too large".into()))?;
        if net <= 0 {
            return Err(Error::Validation(
                "Enter a positive invoice amount before tax".into(),
            ));
        }
        Ok(net)
    }

    pub(super) fn invoice_tax_profile_form(&mut self, ui: &mut egui::Ui, company: i64) {
        egui::CollapsingHeader::new("Calculate tax from a profile").show(ui, |ui| {
            let profiles = match self.store.tax_profiles(company) {
                Ok(profiles) => profiles
                    .into_iter()
                    .filter(|profile| profile.kind == self.invoice_kind)
                    .collect::<Vec<_>>(),
                Err(error) => {
                    self.failure(error);
                    return;
                }
            };
            if profiles.is_empty() {
                ui.label("Set a profile for this invoice type in Tax register.");
                return;
            }
            let selected = profiles
                .iter()
                .find(|profile| Some(profile.id) == self.invoice_tax_profile)
                .map(|profile| profile.name.as_str())
                .unwrap_or("Select profile");
            egui::ComboBox::from_id_salt("invoice-tax-profile")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for profile in &profiles {
                        ui.selectable_value(
                            &mut self.invoice_tax_profile,
                            Some(profile.id),
                            &profile.name,
                        );
                    }
                });
            ui.horizontal_wrapped(|ui| {
                ui.label("Taxable amount");
                ui.add(
                    egui::TextEdit::singleline(&mut self.invoice_taxable_base)
                        .hint_text("All items and adjustments"),
                );
            });
            if ui.button("Calculate and use amounts").clicked() {
                let result = (|| {
                    let profile = self
                        .invoice_tax_profile
                        .ok_or_else(|| Error::Validation("Select a tax profile".into()))?;
                    let net = self.invoice_net_before_tax()?;
                    let whole = self.invoice_taxable_base.trim().is_empty();
                    let base = if whole {
                        net
                    } else {
                        parse_amount(&self.invoice_taxable_base)?
                    };
                    if base > net {
                        return Err(Error::Validation(
                            "Taxable amount exceeds the invoice amount before tax".into(),
                        ));
                    }
                    self.store.calculate_invoice_tax(
                        company,
                        profile,
                        self.invoice_kind,
                        &self.invoice_date,
                        base,
                        whole,
                    )
                })();
                match result {
                    Ok((calculation, mut taxes)) => {
                        let mut taxes = taxes.drain(..);
                        if let Some(primary) = taxes.next() {
                            self.invoice_tax_ledger = Some(primary.ledger_id);
                            self.invoice_tax_amount = format_amount(primary.amount);
                        } else {
                            self.invoice_tax_ledger = None;
                            self.invoice_tax_amount.clear();
                        }
                        self.invoice_additional_taxes = taxes
                            .map(|tax| InvoiceTaxForm {
                                ledger: Some(tax.ledger_id),
                                amount: format_amount(tax.amount),
                                original: None,
                            })
                            .collect();
                        self.invoice_tax_calculation = Some(calculation);
                        self.invoice_tax_calculation_date = self.invoice_date.trim().into();
                        self.message = "Tax amounts calculated".into();
                    }
                    Err(error) => self.failure(error),
                }
            }
        });
        if let Some(calculation) = self.invoice_tax_calculation {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "Rate version {} | Taxable amount {}",
                    calculation.version_id,
                    format_amount(calculation.taxable_amount)
                ));
                if ui.button("Use manual amounts").clicked() {
                    self.invoice_tax_calculation = None;
                    self.invoice_tax_calculation_date.clear();
                }
            });
        }
    }
}
