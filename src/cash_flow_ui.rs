use super::{money, pick_account, App};
use balance::cash_flow::{CashClass, CashFlowReport, CashRole};
use balance::Ledger;
use eframe::egui;

pub(super) struct CashFlowScreen {
    ledger: Option<i64>,
    role: CashRole,
    ledger_reason: String,
    voucher: String,
    class: Option<CashClass>,
    voucher_reason: String,
    from: String,
    to: String,
    report: Option<CashFlowReport>,
}
impl Default for CashFlowScreen {
    fn default() -> Self {
        Self {
            ledger: None,
            role: CashRole::Cash,
            ledger_reason: String::new(),
            voucher: String::new(),
            class: None,
            voucher_reason: String::new(),
            from: String::new(),
            to: String::new(),
            report: None,
        }
    }
}

impl App {
    pub(super) fn cash_flow_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Cash flow");
        let ledgers = match self.store.ledgers(company) {
            Ok(ledgers) => ledgers,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        egui::CollapsingHeader::new("Cash and counterpart ledgers").show(ui, |ui| {
            self.cash_ledger_form(ui, company, &ledgers);
        });
        egui::CollapsingHeader::new("Voucher classifications")
            .open((!self.cash_flow_screen.voucher.trim().is_empty()).then_some(true))
            .show(ui, |ui| self.cash_voucher_form(ui, company));
        ui.separator();
        self.cash_flow_report_view(ui, company);
    }

    fn cash_ledger_form(&mut self, ui: &mut egui::Ui, company: i64, ledgers: &[Ledger]) {
        let form = &mut self.cash_flow_screen;
        let previous = form.ledger;
        pick_account(
            ui,
            "cash-flow-ledger",
            "Ledger",
            ledgers,
            &mut form.ledger,
            false,
        );
        let history = form
            .ledger
            .map(|ledger| self.store.cash_ledger_rules(company, Some(ledger)))
            .transpose();
        let history = match history {
            Ok(history) => history.unwrap_or_default(),
            Err(error) => {
                self.message = error.to_string();
                return;
            }
        };
        if form.ledger != previous {
            form.role = history
                .first()
                .map(|record| record.role)
                .unwrap_or(CashRole::Counterpart(CashClass::Unclassified));
            form.ledger_reason.clear();
        }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("cash-flow-role")
                .selected_text(form.role.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.role, CashRole::Cash, CashRole::Cash.label());
                    for class in CashClass::ALL {
                        ui.selectable_value(
                            &mut form.role,
                            CashRole::Counterpart(class),
                            class.label(),
                        );
                    }
                });
            ui.add(egui::TextEdit::singleline(&mut form.ledger_reason).hint_text("Reason"));
            if ui
                .add_enabled(
                    form.ledger.is_some(),
                    egui::Button::new("Save ledger classification"),
                )
                .clicked()
            {
                if let Some(ledger) = form.ledger {
                    match self.store.save_cash_ledger_rule(
                        company,
                        ledger,
                        form.role,
                        &form.ledger_reason,
                    ) {
                        Ok(()) => {
                            form.ledger_reason.clear();
                            form.report = None;
                            self.message = "Ledger classification saved".into();
                        }
                        Err(error) => self.message = error.to_string(),
                    }
                }
            }
        });
        for record in history {
            ui.small(format!(
                "{} UTC | {} | {}",
                record.recorded_at,
                record.role.label(),
                record.reason
            ));
        }
        match self.store.cash_ledger_rules(company, None) {
            Ok(rules) => {
                for rule in rules.into_iter().filter(|rule| rule.role == CashRole::Cash) {
                    if let Some(ledger) = ledgers.iter().find(|ledger| ledger.id == rule.ledger_id)
                    {
                        ui.label(format!("Cash or bank | {}", ledger.name));
                    }
                }
            }
            Err(error) => self.failure(error),
        }
    }

    fn cash_voucher_form(&mut self, ui: &mut egui::Ui, company: i64) {
        let form = &mut self.cash_flow_screen;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(
                    egui::TextEdit::singleline(&mut form.voucher)
                        .hint_text("Voucher ID")
                        .desired_width(100.0),
                )
                .changed()
            {
                form.class = None;
                form.voucher_reason.clear();
            }
            egui::ComboBox::from_id_salt("voucher-cash-flow-class")
                .selected_text(
                    form.class
                        .map(CashClass::label)
                        .unwrap_or("Use ledger rules"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.class, None, "Use ledger rules");
                    for class in CashClass::ALL {
                        ui.selectable_value(&mut form.class, Some(class), class.label());
                    }
                });
            ui.add(egui::TextEdit::singleline(&mut form.voucher_reason).hint_text("Reason"));
        });
        let id = form.voucher.trim().parse::<i64>().ok();
        if ui
            .add_enabled(
                id.is_some(),
                egui::Button::new("Save voucher classification"),
            )
            .clicked()
        {
            if let Some(id) = id {
                match self.store.save_cash_voucher_rule(
                    company,
                    id,
                    form.class,
                    &form.voucher_reason,
                ) {
                    Ok(()) => {
                        form.voucher_reason.clear();
                        form.report = None;
                        self.message = "Voucher classification saved".into();
                    }
                    Err(error) => self.message = error.to_string(),
                }
            }
        }
        if let Some(id) = id {
            match self.store.cash_voucher_rule_history(company, id) {
                Ok(history) => {
                    for record in history {
                        ui.small(format!(
                            "{} UTC | {} | {}",
                            record.recorded_at,
                            record
                                .class
                                .map(CashClass::label)
                                .unwrap_or("Use ledger rules"),
                            record.reason
                        ));
                    }
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn cash_flow_report_view(&mut self, ui: &mut egui::Ui, company: i64) {
        self.report_export_path_field(ui);
        let form = &mut self.cash_flow_screen;
        ui.small("Uses the current cash accounts and classifications. Mixed postings stay unclassified unless the voucher has a classification.");
        ui.horizontal_wrapped(|ui| {
            for (label, field) in [("From", &mut form.from), ("Through", &mut form.to)] {
                ui.label(label);
                if ui
                    .add(
                        egui::TextEdit::singleline(field)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(105.0),
                    )
                    .changed()
                {
                    form.report = None;
                }
            }
        });
        let from = optional(&form.from);
        let to = optional(&form.to);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Run cash flow report").clicked() {
                match self.store.cash_flow_report(company, from, to) {
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
            if ui.button("Export cash flow CSV").clicked() {
                match self.store.export_cash_flow_csv(
                    company,
                    from,
                    to,
                    self.report_export_path.trim(),
                ) {
                    Ok(()) => self.message = "Cash flow report exported".into(),
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
        let Some(report) = &form.report else { return };
        ui.label(format!(
            "Opening {} | Net movement {} | Closing {}",
            money(report.opening),
            money(report.closing - report.opening),
            money(report.closing)
        ));
        egui::Grid::new("cash-flow-summary")
            .striped(true)
            .show(ui, |ui| {
                for label in ["Classification", "Inflows", "Outflows", "Net"] {
                    ui.strong(label);
                }
                ui.end_row();
                for class in CashClass::ALL {
                    let total = report.totals.get(&class).copied().unwrap_or_default();
                    ui.label(class.label());
                    ui.label(money(total.inflows));
                    ui.label(money(total.outflows));
                    ui.label(money(total.net()));
                    ui.end_row();
                }
            });
        if report.entries.is_empty() {
            ui.label("No cash activity in this period.");
        }
        egui::ScrollArea::both()
            .id_salt("cash-flow-detail")
            .max_height(410.0)
            .show(ui, |ui| {
                egui::Grid::new("cash-flow-vouchers")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in [
                            "Date",
                            "Voucher",
                            "Reference",
                            "Classification",
                            "Counterpart",
                            "Net cash",
                            "Basis",
                        ] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for entry in &report.entries {
                            ui.label(&entry.date);
                            if ui
                                .link(format!("{} {}", entry.kind.as_str(), entry.voucher_id))
                                .clicked()
                            {
                                form.voucher = entry.voucher_id.to_string();
                                form.class = None;
                                form.voucher_reason.clear();
                            }
                            ui.label(&entry.reference);
                            ui.label(entry.class.map(CashClass::label).unwrap_or("Net zero"));
                            ui.label(entry.counterpart.as_deref().unwrap_or("—"));
                            ui.label(money(entry.amount));
                            ui.label(entry.explanation);
                            ui.end_row();
                        }
                    });
            });
    }
}

fn optional(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
