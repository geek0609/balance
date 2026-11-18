use super::{money, pick_account, App};
use balance::payroll::{
    Employee, EmployeeDraft, PayAmount, PayHead, PayHeadDraft, PayHeadKind, PayrollAccrual,
    PayrollDraft,
};
use balance::payroll_payments::PayrollPaymentDraft;
use balance::{format_amount, parse_amount, Category, Error, Ledger, Result};
use eframe::egui;

#[derive(Default)]
struct EmployeeForm {
    code: String,
    name: String,
    department: String,
    title: String,
    start: String,
    end: String,
    payable: Option<i64>,
}
impl EmployeeForm {
    fn draft(&self) -> Result<EmployeeDraft> {
        Ok(EmployeeDraft {
            code: self.code.clone(),
            name: self.name.clone(),
            department: self.department.clone(),
            title: self.title.clone(),
            start_date: optional(&self.start).map(str::to_owned),
            end_date: optional(&self.end).map(str::to_owned),
            payable_ledger_id: self
                .payable
                .ok_or_else(|| Error::Validation("Select a payable ledger".into()))?,
        })
    }
}

struct HeadForm {
    kind: PayHeadKind,
    name: String,
    ledger: Option<i64>,
    liability: Option<i64>,
}
impl Default for HeadForm {
    fn default() -> Self {
        Self {
            kind: PayHeadKind::Earning,
            name: String::new(),
            ledger: None,
            liability: None,
        }
    }
}
impl HeadForm {
    fn draft(&self) -> Result<PayHeadDraft> {
        Ok(PayHeadDraft {
            kind: self.kind,
            name: self.name.clone(),
            ledger_id: self
                .ledger
                .ok_or_else(|| Error::Validation("Select the pay head ledger".into()))?,
            liability_ledger_id: self.liability,
        })
    }
}

#[derive(Default)]
struct PayForm {
    head: Option<i64>,
    amount: String,
}
#[derive(Default)]
struct AccrualForm {
    employee: Option<i64>,
    date: String,
    from: String,
    to: String,
    reference: String,
    narration: String,
    lines: Vec<PayForm>,
}
impl AccrualForm {
    fn draft(&self) -> Result<PayrollDraft> {
        Ok(PayrollDraft {
            employee_id: self
                .employee
                .ok_or_else(|| Error::Validation("Select an employee".into()))?,
            date: self.date.clone(),
            period_start: self.from.clone(),
            period_end: self.to.clone(),
            reference: self.reference.clone(),
            narration: self.narration.clone(),
            amounts: self
                .lines
                .iter()
                .map(|line| {
                    Ok(PayAmount {
                        head_id: line.head.ok_or_else(|| {
                            Error::Validation("Select a head for every amount".into())
                        })?,
                        amount: parse_amount(&line.amount)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

#[derive(Default)]
struct PaymentForm {
    bank: Option<i64>,
    date: String,
    reference: String,
    amount: String,
    narration: String,
}
impl PaymentForm {
    fn draft(&self) -> Result<PayrollPaymentDraft> {
        Ok(PayrollPaymentDraft {
            bank_ledger_id: self
                .bank
                .ok_or_else(|| Error::Validation("Select a cash or bank ledger".into()))?,
            date: self.date.clone(),
            reference: self.reference.clone(),
            amount: parse_amount(&self.amount)?,
            narration: self.narration.clone(),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReversalTarget {
    Payment(i64),
    Accrual(i64),
}
struct PayrollReversalForm {
    target: ReversalTarget,
    date: String,
    reference: String,
    reason: String,
}

#[derive(Default)]
pub(super) struct PayrollScreen {
    employee: EmployeeForm,
    head: HeadForm,
    accrual: AccrualForm,
    register_from: String,
    register_to: String,
    register_employee: Option<i64>,
    entries: Vec<PayrollAccrual>,
    selected: Option<i64>,
    register_loaded: bool,
    statement_path: String,
    payment: PaymentForm,
    reversal: Option<PayrollReversalForm>,
}

impl App {
    pub(super) fn payroll_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Payroll");
        let employees = match self.store.employees(company) {
            Ok(employees) => employees,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let heads = match self.store.pay_heads(company) {
            Ok(heads) => heads,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let ledgers = match self.store.ledgers(company) {
            Ok(ledgers) => ledgers,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let liabilities = ledgers
            .iter()
            .filter(|ledger| ledger.category == Category::Liability)
            .cloned()
            .collect::<Vec<_>>();
        let banks = ledgers
            .iter()
            .filter(|ledger| ledger.category == Category::Asset)
            .cloned()
            .collect::<Vec<_>>();
        let expenses = ledgers
            .into_iter()
            .filter(|ledger| ledger.category == Category::Expense)
            .collect::<Vec<_>>();
        egui::CollapsingHeader::new("Employees").show(ui, |ui| {
            self.payroll_employee_form(ui, company, &liabilities);
            for employee in &employees {
                ui.label(format!(
                    "{} | {} | {} | {} | {}",
                    employee.details.code,
                    employee.details.name,
                    employee.details.department,
                    employee.details.title,
                    employee.payable_ledger
                ));
                if employee.details.start_date.is_some() || employee.details.end_date.is_some() {
                    ui.small(format!(
                        "Employment {} to {}",
                        employee.details.start_date.as_deref().unwrap_or("—"),
                        employee.details.end_date.as_deref().unwrap_or("—")
                    ));
                }
            }
        });
        egui::CollapsingHeader::new("Pay heads").show(ui, |ui| {
            self.pay_head_form(ui, company, &expenses, &liabilities);
            for head in &heads {
                let offset = head
                    .liability_ledger
                    .as_ref()
                    .map(|name| format!(" | {name}"))
                    .unwrap_or_default();
                ui.label(format!(
                    "{} | {} | {}{offset}",
                    head.details.name,
                    head.details.kind.label(),
                    head.ledger
                ));
            }
        });
        egui::CollapsingHeader::new("Record payroll accrual")
            .default_open(true)
            .show(ui, |ui| {
                self.payroll_accrual_form(ui, company, &employees, &heads);
            });
        egui::CollapsingHeader::new("Payroll register").show(ui, |ui| {
            self.payroll_register_view(ui, company, &employees, &banks);
        });
    }

    fn payroll_employee_form(&mut self, ui: &mut egui::Ui, company: i64, liabilities: &[Ledger]) {
        let form = &mut self.payroll_screen.employee;
        egui::Grid::new("employee-fields").show(ui, |ui| {
            for (label, value) in [
                ("Code", &mut form.code),
                ("Name", &mut form.name),
                ("Department", &mut form.department),
                ("Title", &mut form.title),
                ("Start date (optional)", &mut form.start),
                ("End date (optional)", &mut form.end),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        pick_account(
            ui,
            "employee-payable",
            "Payable ledger",
            liabilities,
            &mut form.payable,
            false,
        );
        if ui.button("Create employee").clicked() {
            match form
                .draft()
                .and_then(|draft| self.store.create_employee(company, &draft))
            {
                Ok(id) => {
                    *form = EmployeeForm::default();
                    self.message = format!("Employee {id} created");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn pay_head_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        expenses: &[Ledger],
        liabilities: &[Ledger],
    ) {
        let form = &mut self.payroll_screen.head;
        let previous = form.kind;
        egui::ComboBox::from_id_salt("head-kind")
            .selected_text(form.kind.label())
            .show_ui(ui, |ui| {
                for kind in PayHeadKind::ALL {
                    ui.selectable_value(&mut form.kind, kind, kind.label());
                }
            });
        if previous != form.kind {
            form.ledger = None;
            form.liability = None;
        }
        ui.add(egui::TextEdit::singleline(&mut form.name).hint_text("Pay head name"));
        let (label, ledgers) = if form.kind == PayHeadKind::Deduction {
            ("Liability ledger", liabilities)
        } else {
            ("Expense ledger", expenses)
        };
        pick_account(ui, "head-ledger", label, ledgers, &mut form.ledger, false);
        if form.kind == PayHeadKind::EmployerContribution {
            pick_account(
                ui,
                "head-offset",
                "Liability ledger",
                liabilities,
                &mut form.liability,
                false,
            );
        }
        if ui.button("Create pay head").clicked() {
            match form
                .draft()
                .and_then(|draft| self.store.create_pay_head(company, &draft))
            {
                Ok(id) => {
                    *form = HeadForm::default();
                    self.message = format!("Pay head {id} created");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn payroll_accrual_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        employees: &[Employee],
        heads: &[PayHead],
    ) {
        if employees.is_empty() || heads.is_empty() {
            ui.label("Create an employee and pay heads first.");
            return;
        }
        ui.small("Enter the amounts for this employee. Posting records payroll expenses and liabilities.");
        let form = &mut self.payroll_screen.accrual;
        pick_employee(ui, "accrual-employee", employees, &mut form.employee, false);
        egui::Grid::new("payroll-accrual-fields").show(ui, |ui| {
            for (label, value) in [
                ("Posting date (YYYY-MM-DD)", &mut form.date),
                ("Period start (YYYY-MM-DD)", &mut form.from),
                ("Period end (YYYY-MM-DD)", &mut form.to),
                ("Reference", &mut form.reference),
                ("Narration", &mut form.narration),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let mut remove = None;
        for (index, row) in form.lines.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let current = heads
                    .iter()
                    .find(|head| Some(head.id) == row.head)
                    .map(|head| head.details.name.as_str())
                    .unwrap_or("Select pay head");
                egui::ComboBox::from_id_salt(("accrual-head", index))
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for head in heads {
                            ui.selectable_value(
                                &mut row.head,
                                Some(head.id),
                                format!("{} | {}", head.details.name, head.details.kind.label()),
                            );
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut row.amount)
                        .hint_text("Amount")
                        .desired_width(110.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            form.lines.remove(index);
        }
        if ui.button("Add pay amount").clicked() {
            form.lines.push(PayForm::default());
        }
        if ui.button("Post payroll accrual").clicked() {
            match form
                .draft()
                .and_then(|draft| self.store.post_payroll_accrual(company, &draft))
            {
                Ok(id) => {
                    form.reference.clear();
                    form.narration.clear();
                    form.lines.clear();
                    self.payroll_screen.entries.clear();
                    self.payroll_screen.register_loaded = false;
                    self.message = format!("Payroll accrual {id} posted");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn payroll_register_view(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        employees: &[Employee],
        banks: &[Ledger],
    ) {
        self.report_export_path_field(ui);
        let form = &mut self.payroll_screen;
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            for (label, value) in [
                ("Posting from", &mut form.register_from),
                ("Through", &mut form.register_to),
            ] {
                ui.label(label);
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(value)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(105.0),
                    )
                    .changed();
            }
        });
        let original = form.register_employee;
        pick_employee(
            ui,
            "register-employee",
            employees,
            &mut form.register_employee,
            true,
        );
        if changed || original != form.register_employee {
            form.register_loaded = false;
            form.entries.clear();
            form.selected = None;
            form.payment = PaymentForm::default();
            form.reversal = None;
        }
        let from = optional(&form.register_from);
        let to = optional(&form.register_to);
        if ui.button("Run payroll register").clicked() {
            match self
                .store
                .payroll_register(company, from, to, form.register_employee)
            {
                Ok(rows) => {
                    form.entries = rows;
                    form.register_loaded = true;
                    self.message.clear();
                }
                Err(error) => {
                    form.register_loaded = false;
                    self.message = error.to_string();
                }
            }
        }
        if ui.button("Export payroll register CSV").clicked() {
            match self.store.export_payroll_register_csv(
                company,
                from,
                to,
                form.register_employee,
                self.report_export_path.trim(),
            ) {
                Ok(()) => self.message = "Payroll register exported".into(),
                Err(error) => self.message = error.to_string(),
            }
        }
        if !form.register_loaded {
            return;
        }
        if form.entries.is_empty() {
            ui.label("No payroll entries match.");
        }
        let mut net = 0_i128;
        let mut paid = 0_i128;
        let mut outstanding = 0_i128;
        for row in &form.entries {
            if row.reversal.is_none() {
                net += i128::from(row.net);
                paid += row.paid;
                outstanding += row.outstanding;
            }
            let selected = form.selected == Some(row.id);
            if ui
                .selectable_label(
                    selected,
                    format!(
                        "{} | {} {} | {} | {}",
                        row.date,
                        row.code,
                        row.employee,
                        row.reference,
                        if row.reversal.is_some() {
                            "Reversed".into()
                        } else {
                            format!("Outstanding {}", money(row.outstanding))
                        }
                    ),
                )
                .clicked()
            {
                form.selected = (!selected).then_some(row.id);
                form.payment = PaymentForm {
                    amount: money(row.outstanding),
                    ..PaymentForm::default()
                };
                form.reversal = None;
            }
        }
        ui.label(format!(
            "Net accrued {} | Paid {} | Outstanding {}",
            money(net),
            money(paid),
            money(outstanding)
        ));
        let selected = form
            .entries
            .iter()
            .find(|row| Some(row.id) == form.selected)
            .cloned();
        let cutoff = to.map(str::to_owned);
        if let Some(row) = selected {
            ui.separator();
            self.payroll_detail(ui, company, &row, cutoff.as_deref(), banks);
        }
    }

    fn payroll_detail(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        row: &PayrollAccrual,
        end: Option<&str>,
        banks: &[Ledger],
    ) {
        crate::theme::section(ui, format!("{} | {}", row.employee, row.reference));
        ui.label(format!(
            "Period {} to {} | Voucher {}",
            row.period_start, row.period_end, row.voucher_id
        ));
        ui.label(format!(
            "Gross {} | Deductions {} | Net {} | Employer contributions {}",
            money(row.gross.into()),
            money(row.deductions.into()),
            money(row.net.into()),
            money(row.employer_contributions.into())
        ));
        if !row.narration.is_empty() {
            ui.label(&row.narration);
        }
        match self.store.payroll_lines(company, row.id) {
            Ok(lines) => {
                for line in &lines {
                    ui.label(format!(
                        "{} | {} | {}",
                        line.head,
                        line.kind.label(),
                        money(line.amount.into())
                    ));
                }
                if let Some(reversal) = &row.reversal {
                    ui.label(format!(
                        "Accrual reversed {} | {} (voucher {}) | {}",
                        reversal.date, reversal.reference, reversal.voucher_id, reversal.reason
                    ));
                    if end.is_none() && ui.button("Prepare replacement accrual").clicked() {
                        self.payroll_screen.accrual = AccrualForm {
                            employee: Some(row.employee_id),
                            date: reversal.date.clone(),
                            from: row.period_start.clone(),
                            to: row.period_end.clone(),
                            reference: String::new(),
                            narration: row.narration.clone(),
                            lines: lines
                                .iter()
                                .map(|line| PayForm {
                                    head: Some(line.head_id),
                                    amount: format_amount(line.amount),
                                })
                                .collect(),
                        };
                        self.message =
                            "Replacement prepared. Check the amounts and enter a new reference."
                                .into();
                    }
                }
            }
            Err(error) => self.failure(error),
        }
        if end.is_none() && row.reversal.is_none() {
            self.payroll_reversal_form(ui, company, ReversalTarget::Accrual(row.id), row.paid == 0);
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.payroll_screen.statement_path)
                    .hint_text("Absolute path to a new .html file")
                    .desired_width(300.0),
            );
            if ui.button("Export printable pay statement").clicked() {
                match self.store.export_pay_statement_html(
                    company,
                    row.id,
                    end,
                    self.payroll_screen.statement_path.trim(),
                ) {
                    Ok(()) => self.message = "Pay statement created".into(),
                    Err(error) => self.failure(error),
                }
            }
        });
        ui.strong("Salary payments");
        match self.store.payroll_payments(company, row.id, end) {
            Ok(payments) if payments.is_empty() => {
                ui.label("No payments recorded through this date.");
            }
            Ok(payments) => {
                for payment in payments {
                    ui.label(format!(
                        "{} | {} | {} | {} | Voucher {}",
                        payment.date,
                        payment.reference,
                        payment.bank,
                        money(payment.amount.into()),
                        payment.voucher_id
                    ));
                    if let Some(reversal) = &payment.reversal {
                        ui.small(format!(
                            "Reversed {} | {} (voucher {}) | {}",
                            reversal.date, reversal.reference, reversal.voucher_id, reversal.reason
                        ));
                    } else if end.is_none() {
                        ui.push_id(payment.id, |ui| {
                            self.payroll_reversal_form(
                                ui,
                                company,
                                ReversalTarget::Payment(payment.id),
                                true,
                            )
                        });
                    }
                }
            }
            Err(error) => self.failure(error),
        }
        if end.is_some() {
            ui.small(
                "Clear the Through date and run the register again to enter a payment or reversal.",
            );
            return;
        }
        if row.outstanding <= 0 {
            return;
        }
        egui::CollapsingHeader::new("Record salary payment")
            .id_salt(row.id)
            .show(ui, |ui| {
                let form = &mut self.payroll_screen.payment;
                pick_account(
                    ui,
                    "salary-bank-ledger",
                    "Cash or bank",
                    banks,
                    &mut form.bank,
                    false,
                );
                egui::Grid::new("salary-payment-fields").show(ui, |ui| {
                    for (label, field) in [
                        ("Date (YYYY-MM-DD)", &mut form.date),
                        ("Reference", &mut form.reference),
                        ("Amount", &mut form.amount),
                        ("Narration", &mut form.narration),
                    ] {
                        ui.label(label);
                        ui.text_edit_singleline(field);
                        ui.end_row();
                    }
                });
                if ui.button("Post salary payment").clicked() {
                    match form
                        .draft()
                        .and_then(|draft| self.store.pay_payroll_accrual(company, row.id, &draft))
                    {
                        Ok(payment) => {
                            *form = PaymentForm::default();
                            self.reload_payroll_after_change(
                                company,
                                format!("Salary payment {payment} recorded"),
                            );
                        }
                        Err(error) => self.failure(error),
                    }
                }
            });
    }

    fn payroll_reversal_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        target: ReversalTarget,
        enabled: bool,
    ) {
        let label = match target {
            ReversalTarget::Accrual(_) => "Reverse accrual",
            ReversalTarget::Payment(_) => "Reverse payment",
        };
        if ui
            .add_enabled(enabled, egui::Button::new(label))
            .on_disabled_hover_text("Reverse active salary payments first.")
            .clicked()
        {
            self.payroll_screen.reversal = Some(PayrollReversalForm {
                target,
                date: String::new(),
                reference: String::new(),
                reason: String::new(),
            });
        }
        let Some(form) = &mut self.payroll_screen.reversal else {
            return;
        };
        if form.target != target {
            return;
        }
        egui::Grid::new("payroll-reversal-fields").show(ui, |ui| {
            for (label, field) in [
                ("Date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(field);
                ui.end_row();
            }
        });
        let mut post = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            post = ui
                .add_enabled(enabled, egui::Button::new("Post reversal"))
                .clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if post {
            let result = match target {
                ReversalTarget::Accrual(accrual) => self.store.reverse_payroll_accrual(
                    company,
                    accrual,
                    &form.date,
                    &form.reference,
                    &form.reason,
                ),
                ReversalTarget::Payment(payment) => self.store.reverse_payroll_payment(
                    company,
                    payment,
                    &form.date,
                    &form.reference,
                    &form.reason,
                ),
            };
            match result {
                Ok(voucher) => {
                    cancel = true;
                    self.reload_payroll_after_change(
                        company,
                        format!("Reversal voucher {voucher} recorded"),
                    );
                }
                Err(error) => self.failure(error),
            }
        }
        if cancel {
            self.payroll_screen.reversal = None;
        }
    }

    fn reload_payroll_after_change(&mut self, company: i64, success: String) {
        let screen = &mut self.payroll_screen;
        match self.store.payroll_register(
            company,
            optional(&screen.register_from),
            optional(&screen.register_to),
            screen.register_employee,
        ) {
            Ok(entries) => {
                screen.entries = entries;
                self.message = success;
            }
            Err(error) => {
                screen.register_loaded = false;
                self.message = format!("{success}. Could not refresh the register: {error}");
            }
        }
    }
}

fn pick_employee(
    ui: &mut egui::Ui,
    id: &str,
    employees: &[Employee],
    selected: &mut Option<i64>,
    all: bool,
) {
    let fallback = if all {
        "All employees"
    } else {
        "Select employee"
    };
    let name = employees
        .iter()
        .find(|row| Some(row.id) == *selected)
        .map(|row| format!("{} | {}", row.details.code, row.details.name))
        .unwrap_or_else(|| fallback.into());
    egui::ComboBox::from_id_salt(id)
        .selected_text(name)
        .show_ui(ui, |ui| {
            if all {
                ui.selectable_value(selected, None, fallback);
            }
            for employee in employees {
                ui.selectable_value(
                    selected,
                    Some(employee.id),
                    format!("{} | {}", employee.details.code, employee.details.name),
                );
            }
        });
}

fn optional(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
