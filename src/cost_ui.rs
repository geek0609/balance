use super::{money, App};
use balance::costs::{CostCentre, CostPosting, CostShare, CostTotals};
use balance::{format_amount, parse_amount, Error, Result};
use eframe::egui;

#[derive(Default, PartialEq, Eq)]
struct ShareForm {
    centre: Option<i64>,
    amount: String,
}
impl ShareForm {
    fn from_share(share: &CostShare) -> Self {
        Self {
            centre: Some(share.centre_id),
            amount: format_amount(share.amount),
        }
    }
}

struct AllocationForm {
    posting: CostPosting,
    original: Vec<CostShare>,
    shares: Vec<ShareForm>,
    reason: String,
}
impl AllocationForm {
    fn has_changes(&self) -> bool {
        !self.reason.trim().is_empty()
            || self.shares.len() != self.original.len()
            || self
                .shares
                .iter()
                .zip(&self.original)
                .any(|(form, original)| *form != ShareForm::from_share(original))
    }

    fn shares(&self) -> Result<Vec<CostShare>> {
        self.shares
            .iter()
            .map(|row| {
                Ok(CostShare {
                    centre_id: row.centre.ok_or_else(|| {
                        Error::Validation("Select a centre for each allocation".into())
                    })?,
                    amount: parse_amount(&row.amount)?,
                })
            })
            .collect()
    }
}

#[derive(Default)]
pub(super) struct CostScreen {
    name: String,
    parent: Option<i64>,
    search: String,
    allocation: Option<AllocationForm>,
    from: String,
    to: String,
}
impl CostScreen {
    pub(super) fn has_pending_changes(&self) -> bool {
        self.allocation
            .as_ref()
            .is_some_and(AllocationForm::has_changes)
    }
}

impl App {
    pub(super) fn cost_page(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.heading("Cost centres");
        let centres = match self.store.cost_centres(company) {
            Ok(centres) => centres,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        egui::CollapsingHeader::new("Create cost centre").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.cost_screen.name).hint_text("Centre name"),
                );
                pick_centre(
                    ui,
                    "cost-parent",
                    &centres,
                    &mut self.cost_screen.parent,
                    "No parent",
                );
                if ui.button("Create").clicked() {
                    match self.store.create_cost_centre(
                        company,
                        &self.cost_screen.name,
                        self.cost_screen.parent,
                    ) {
                        Ok(id) => {
                            self.cost_screen.name.clear();
                            self.message = format!("Cost centre {id} created");
                        }
                        Err(error) => self.failure(error),
                    }
                }
            });
        });
        ui.separator();
        egui::CollapsingHeader::new("Allocate a posting")
            .default_open(true)
            .show(ui, |ui| {
                if self.cost_screen.allocation.is_some() {
                    self.cost_allocation_form(ui, company, &centres);
                } else {
                    self.cost_posting_picker(ui, company);
                }
            });
        ui.separator();
        egui::CollapsingHeader::new("Income and expenses by cost centre").show(ui, |ui| {
            self.cost_report_view(ui, company);
        });
    }

    fn cost_posting_picker(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.add(
            egui::TextEdit::singleline(&mut self.cost_screen.search)
                .hint_text("Voucher ID, reference or ledger"),
        );
        ui.small("Showing up to 100 income and expense postings.");
        let postings = match self.store.cost_postings(company, &self.cost_screen.search) {
            Ok(postings) => postings,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        if postings.is_empty() {
            ui.label("No matching postings.");
        }
        egui::ScrollArea::vertical()
            .id_salt("cost-posting-picker")
            .max_height(240.0)
            .show(ui, |ui| {
                for posting in postings {
                    let reference = if posting.reference.is_empty() {
                        String::new()
                    } else {
                        format!(" | {}", posting.reference)
                    };
                    let caption = format!(
                        "{} | Voucher {}{} | {} | {}",
                        posting.date,
                        posting.voucher_id,
                        reference,
                        posting.ledger,
                        posting_value(posting.amount)
                    );
                    if ui.selectable_label(false, caption).clicked() {
                        match self.store.posting_cost_shares(company, posting.id) {
                            Ok(original) => {
                                self.cost_screen.allocation = Some(AllocationForm {
                                    posting,
                                    shares: original.iter().map(ShareForm::from_share).collect(),
                                    original,
                                    reason: String::new(),
                                });
                                self.message.clear();
                            }
                            Err(error) => self.failure(error),
                        }
                    }
                }
            });
    }

    fn cost_allocation_form(&mut self, ui: &mut egui::Ui, company: i64, centres: &[CostCentre]) {
        let Some(form) = &mut self.cost_screen.allocation else {
            return;
        };
        ui.label(format!(
            "{} | Voucher {} | {} | {}",
            form.posting.date,
            form.posting.voucher_id,
            form.posting.ledger,
            posting_value(form.posting.amount)
        ));
        ui.small("Enter positive amounts. The posting supplies the debit or credit direction. Any remainder stays unallocated.");
        let mut remove = None;
        for (index, share) in form.shares.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                pick_centre(
                    ui,
                    ("cost-share", index),
                    centres,
                    &mut share.centre,
                    "Select cost centre",
                );
                ui.add(
                    egui::TextEdit::singleline(&mut share.amount)
                        .hint_text("Amount")
                        .desired_width(90.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            form.shares.remove(index);
        }
        if ui.button("Add split").clicked() {
            form.shares.push(ShareForm::default());
        }
        ui.add(egui::TextEdit::singleline(&mut form.reason).hint_text("Reason for change"));
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            save = ui
                .add_enabled(
                    form.has_changes() && !form.reason.trim().is_empty(),
                    egui::Button::new("Save allocation"),
                )
                .clicked();
            cancel = ui.button("Back to postings").clicked();
        });
        if cancel {
            self.cost_screen.allocation = None;
            self.message.clear();
            return;
        }
        let voucher = form.posting.voucher_id;
        if save {
            match form.shares().and_then(|shares| {
                self.store
                    .save_cost_shares(
                        company,
                        &form.posting,
                        &form.original,
                        &shares,
                        &form.reason,
                    )
                    .map(|()| shares)
            }) {
                Ok(mut shares) => {
                    shares.sort_by_key(|share| share.centre_id);
                    form.shares = shares.iter().map(ShareForm::from_share).collect();
                    form.original = shares;
                    form.reason.clear();
                    self.message = "Cost allocation saved".into();
                }
                Err(error) => self.failure(error),
            }
        }
        egui::CollapsingHeader::new("Allocation history")
            .id_salt(("cost-history", voucher))
            .show(ui, |ui| match self.store.cost_history(company, voucher) {
                Ok(history) if history.is_empty() => {
                    ui.label("No allocation changes for this voucher.");
                }
                Ok(history) => {
                    for change in history {
                        ui.label(format!(
                            "{} UTC | posting {} | {} | {} | {}",
                            change.changed_at,
                            change.posting_id,
                            change.ledger,
                            posting_value(change.posting_amount),
                            change.reason
                        ));
                        if change.shares.is_empty() {
                            ui.small("Unallocated");
                        }
                        for share in change.shares {
                            let name = centres
                                .iter()
                                .find(|centre| centre.id == share.centre_id)
                                .map(|centre| centre.name.as_str())
                                .unwrap_or("Unknown centre");
                            ui.small(format!("{name} | {}", format_amount(share.amount)));
                        }
                    }
                }
                Err(error) => self.failure(error),
            });
    }

    fn cost_report_view(&mut self, ui: &mut egui::Ui, company: i64) {
        self.report_export_path_field(ui);
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(
                egui::TextEdit::singleline(&mut self.cost_screen.from)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
            ui.label("Through");
            ui.add(
                egui::TextEdit::singleline(&mut self.cost_screen.to)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(100.0),
            );
        });
        let from =
            (!self.cost_screen.from.trim().is_empty()).then_some(self.cost_screen.from.trim());
        let to = (!self.cost_screen.to.trim().is_empty()).then_some(self.cost_screen.to.trim());
        if ui.button("Export cost report as CSV").clicked() {
            match self.store.export_cost_report_csv(
                company,
                from,
                to,
                self.report_export_path.trim(),
            ) {
                Ok(()) => self.message = "Cost centre report exported".into(),
                Err(error) => self.message = error.to_string(),
            }
        }
        match self.store.cost_report(company, from, to) {
            Ok(report) => {
                ui.small("The report uses saved allocations. The final columns include subcentres, so those columns overlap.");
                egui::ScrollArea::horizontal()
                    .id_salt("cost-totals-scroll")
                    .show(ui, |ui| {
                        egui::Grid::new("cost-totals").striped(true).show(ui, |ui| {
                            for label in [
                                "Centre",
                                "Income",
                                "Expenses",
                                "Net",
                                "Income with children",
                                "Expenses with children",
                                "Net with children",
                            ] {
                                ui.strong(label);
                            }
                            ui.end_row();
                            for entry in report.rows {
                                ui.label(entry.path);
                                totals(ui, entry.direct);
                                totals(ui, entry.including_children);
                                ui.end_row();
                            }
                            ui.strong("Unallocated");
                            totals(ui, report.unallocated);
                            ui.end_row();
                            ui.strong("Company total");
                            totals(ui, report.total);
                            ui.end_row();
                        });
                    });
            }
            Err(error) => self.failure(error),
        }
    }
}

fn totals(ui: &mut egui::Ui, value: CostTotals) {
    ui.label(money(value.income));
    ui.label(money(value.expenses));
    ui.label(money(value.net()));
}

fn posting_value(amount: i64) -> String {
    let signed = i128::from(amount);
    let side = if signed < 0 { "Cr" } else { "Dr" };
    format!("{} {side}", money(signed.abs()))
}

fn pick_centre(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    centres: &[CostCentre],
    current: &mut Option<i64>,
    prompt: &str,
) {
    let selected = centres
        .iter()
        .find(|centre| Some(centre.id) == *current)
        .map(|centre| centre.name.as_str())
        .unwrap_or(prompt);
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(current, None, prompt);
            for centre in centres {
                ui.selectable_value(current, Some(centre.id), &centre.name);
            }
        });
}
