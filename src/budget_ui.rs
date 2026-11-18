use super::{money, pick_account, App};
use balance::budgets::{BudgetDraft, BudgetReport, BudgetTarget, BudgetVersion};
use balance::costs::CostCentre;
use balance::{format_amount, parse_amount, Category, Error, Ledger, Result};
use eframe::egui;

#[derive(Default)]
struct TargetForm {
    ledger: Option<i64>,
    amount: String,
}

#[derive(Default)]
struct BudgetForm {
    name: String,
    from: String,
    to: String,
    reason: String,
    centre: Option<i64>,
    include_children: bool,
    targets: Vec<TargetForm>,
}
impl BudgetForm {
    fn draft(&self) -> Result<BudgetDraft> {
        Ok(BudgetDraft {
            name: self.name.clone(),
            start_date: self.from.clone(),
            end_date: self.to.clone(),
            centre_id: self.centre,
            include_children: self.include_children,
            reason: self.reason.clone(),
            targets: self
                .targets
                .iter()
                .map(|target| {
                    Ok(BudgetTarget {
                        ledger_id: target.ledger.ok_or_else(|| {
                            Error::Validation("Select a ledger for each target".into())
                        })?,
                        amount: parse_amount(&target.amount)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        })
    }

    fn from_report(report: &BudgetReport) -> Self {
        Self {
            name: report.version.name.clone(),
            from: report.version.start_date.clone(),
            to: report.version.end_date.clone(),
            reason: String::new(),
            centre: report.version.centre_id,
            include_children: report.version.include_children,
            targets: report
                .rows
                .iter()
                .filter_map(|row| {
                    row.planned.map(|amount| TargetForm {
                        ledger: Some(row.ledger_id),
                        amount: format_amount(amount),
                    })
                })
                .collect(),
        }
    }
}

#[derive(Default)]
pub(super) struct BudgetScreen {
    form: BudgetForm,
    older_versions: bool,
    report: Option<BudgetReport>,
}

impl App {
    pub(super) fn budgets_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Budgets");
        egui::CollapsingHeader::new("Set a budget")
            .default_open(true)
            .show(ui, |ui| {
                self.budget_form(ui, company);
            });
        ui.separator();
        let versions = match self.store.budget_versions(company) {
            Ok(versions) => versions,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        if versions.is_empty() {
            ui.label("No budgets saved.");
            return;
        }
        ui.checkbox(
            &mut self.budget_screen.older_versions,
            "Show earlier versions",
        );
        egui::ScrollArea::vertical()
            .id_salt("budget-version-list")
            .max_height(175.0)
            .show(ui, |ui| {
                for version in versions
                    .iter()
                    .filter(|version| version.latest || self.budget_screen.older_versions)
                {
                    let selected = self
                        .budget_screen
                        .report
                        .as_ref()
                        .is_some_and(|report| report.version.id == version.id);
                    if ui
                        .selectable_label(selected, version_label(version))
                        .clicked()
                    {
                        match self.store.budget_report(company, version.id) {
                            Ok(report) => {
                                self.budget_screen.report = Some(report);
                                self.message.clear();
                            }
                            Err(error) => self.message = error.to_string(),
                        }
                    }
                }
            });
        self.budget_report_view(ui, company);
    }

    fn budget_form(&mut self, ui: &mut egui::Ui, company: i64) {
        let ledgers = match self.store.ledgers(company) {
            Ok(ledgers) => ledgers
                .into_iter()
                .filter(|ledger| matches!(ledger.category, Category::Income | Category::Expense))
                .collect::<Vec<Ledger>>(),
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let centres = match self.store.cost_centres(company) {
            Ok(centres) => centres,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let form = &mut self.budget_screen.form;
        egui::Grid::new("budget-fields").show(ui, |ui| {
            for (label, value) in [
                ("Budget name", &mut form.name),
                ("Start (YYYY-MM-DD)", &mut form.from),
                ("End (YYYY-MM-DD)", &mut form.to),
                ("Reason for this version", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        budget_scope(ui, &centres, form);
        let mut remove = None;
        for (index, target) in form.targets.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                pick_account(
                    ui,
                    ("budget-target", index),
                    "Ledger",
                    &ledgers,
                    &mut target.ledger,
                    false,
                );
                ui.add(
                    egui::TextEdit::singleline(&mut target.amount)
                        .hint_text("Target amount")
                        .desired_width(110.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            form.targets.remove(index);
        }
        ui.horizontal(|ui| {
            if ui.button("Add ledger target").clicked() {
                form.targets.push(TargetForm::default());
            }
            if ui.button("Save budget version").clicked() {
                match form
                    .draft()
                    .and_then(|draft| self.store.save_budget_version(company, &draft))
                {
                    Ok(id) => {
                        *form = BudgetForm::default();
                        match self.store.budget_report(company, id) {
                            Ok(report) => {
                                self.budget_screen.report = Some(report);
                                self.message = format!("Budget version {id} saved");
                            }
                            Err(error) => {
                                self.message = format!(
                                    "Budget version {id} saved. Could not load report: {error}"
                                )
                            }
                        }
                    }
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
    }

    fn budget_report_view(&mut self, ui: &mut egui::Ui, company: i64) {
        let Some(version) = self
            .budget_screen
            .report
            .as_ref()
            .map(|report| report.version.id)
        else {
            return;
        };
        ui.separator();
        self.report_export_path_field(ui);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Refresh actuals").clicked() {
                match self.store.budget_report(company, version) {
                    Ok(report) => {
                        self.budget_screen.report = Some(report);
                        self.message.clear();
                    }
                    Err(error) => self.failure(error),
                }
            }
            if ui.button("Export budget report CSV").clicked() {
                match self
                    .store
                    .export_budget_csv(company, version, self.report_export_path.trim())
                {
                    Ok(()) => self.message = "Budget report exported".into(),
                    Err(error) => self.failure(error),
                }
            }
        });
        let Some(report) = self.budget_screen.report.as_ref() else {
            return;
        };
        ui.heading(version_label(&report.version));
        ui.label(format!(
            "{} UTC | {}",
            report.version.recorded_at, report.version.reason
        ));
        ui.small("Actuals use saved postings and current allocations. Unbudgeted activity has a zero target in the totals.");
        if ui
            .button("Copy this version into the budget form")
            .clicked()
        {
            self.budget_screen.form = BudgetForm::from_report(report);
        }
        egui::ScrollArea::both()
            .id_salt("budget-actual-table")
            .max_height(425.0)
            .show(ui, |ui| {
                egui::Grid::new("budget-actuals")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in [
                            "Ledger",
                            "Category",
                            "Target",
                            "Actual",
                            "Favourable variance",
                        ] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for row in &report.rows {
                            ui.label(&row.ledger);
                            ui.label(row.category.as_str());
                            ui.label(
                                row.planned
                                    .map(|amount| money(amount.into()))
                                    .unwrap_or_else(|| "No target".into()),
                            );
                            ui.label(money(row.actual));
                            ui.label(money(row.favourable_variance()));
                            ui.end_row();
                        }
                        for (label, target, actual, variance) in [
                            (
                                "Total income",
                                report.planned.income,
                                report.actual.income,
                                report.actual.income - report.planned.income,
                            ),
                            (
                                "Total expenses",
                                report.planned.expenses,
                                report.actual.expenses,
                                report.planned.expenses - report.actual.expenses,
                            ),
                            (
                                "Net",
                                report.planned.net(),
                                report.actual.net(),
                                report.actual.net() - report.planned.net(),
                            ),
                        ] {
                            ui.strong(label);
                            ui.label("");
                            ui.label(money(target));
                            ui.label(money(actual));
                            ui.label(money(variance));
                            ui.end_row();
                        }
                    });
            });
    }
}

fn budget_scope(ui: &mut egui::Ui, centres: &[CostCentre], form: &mut BudgetForm) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Scope");
        let name = centres
            .iter()
            .find(|centre| Some(centre.id) == form.centre)
            .map(|centre| centre.name.as_str())
            .unwrap_or("Whole company");
        egui::ComboBox::from_id_salt("budget-cost-centre")
            .selected_text(name)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut form.centre, None, "Whole company");
                for centre in centres {
                    ui.selectable_value(&mut form.centre, Some(centre.id), &centre.name);
                }
            });
        if form.centre.is_some() {
            ui.checkbox(&mut form.include_children, "Include subcentres");
        } else {
            form.include_children = false;
        }
    });
}

fn version_label(version: &BudgetVersion) -> String {
    let suffix = if version.latest { " (latest)" } else { "" };
    format!(
        "{} v{}{suffix} | {} to {} | {}",
        version.name,
        version.id,
        version.start_date,
        version.end_date,
        version.scope_label()
    )
}
