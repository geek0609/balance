use super::{money, pick_batch, pick_location, pick_stock, App};
use balance::inventory::{format_quantity, parse_quantity, StockItem, StockLocation};
use balance::manufacturing::{
    ComponentDraft, ComponentRequirement, MaterialDraft, MaterialRevision, ProductionDraft,
    StockBucket,
};
use balance::{parse_amount, Error, Result};
use eframe::egui;
use std::collections::HashMap;

#[derive(Default)]
struct ComponentForm {
    item: Option<i64>,
    quantity: String,
}

#[derive(Default)]
struct BillForm {
    output: Option<i64>,
    name: String,
    quantity: String,
    reason: String,
    components: Vec<ComponentForm>,
}
impl BillForm {
    fn draft(&self) -> Result<MaterialDraft> {
        let output_item_id = self
            .output
            .ok_or_else(|| Error::Validation("Select the finished item".into()))?;
        let components = self
            .components
            .iter()
            .map(|row| {
                Ok(ComponentDraft {
                    item_id: row.item.ok_or_else(|| {
                        Error::Validation("Select an item for every component".into())
                    })?,
                    quantity: parse_quantity(&row.quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(MaterialDraft {
            output_item_id,
            name: self.name.clone(),
            output_quantity: parse_quantity(&self.quantity)?,
            reason: self.reason.clone(),
            components,
        })
    }
}

#[derive(Default)]
struct BucketForm {
    location: Option<i64>,
    batch: Option<i64>,
}
impl BucketForm {
    fn bucket(&self, label: &str) -> Result<StockBucket> {
        let location_id = self
            .location
            .ok_or_else(|| Error::Validation(format!("Select a location for {label}")))?;
        Ok(StockBucket {
            location_id,
            batch_id: self.batch,
        })
    }
}

struct ConsumptionForm {
    requirement: ComponentRequirement,
    bucket: BucketForm,
}

#[derive(Default)]
struct RunForm {
    revision: Option<i64>,
    quantity: String,
    requirements: Vec<ConsumptionForm>,
    date: String,
    reference: String,
    narration: String,
    cost: String,
    output: BucketForm,
}
impl RunForm {
    fn draft(&self) -> Result<ProductionDraft> {
        let revision_id = self
            .revision
            .ok_or_else(|| Error::Validation("Select a bill of materials version".into()))?;
        let components = self
            .requirements
            .iter()
            .map(|row| {
                Ok((
                    row.requirement.item_id,
                    row.bucket.bucket(&row.requirement.item)?,
                ))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        Ok(ProductionDraft {
            revision_id,
            date: self.date.clone(),
            reference: self.reference.clone(),
            narration: self.narration.clone(),
            quantity: parse_quantity(&self.quantity)?,
            output: self.output.bucket("the finished item")?,
            components,
            acquisition_cost: (!self.cost.trim().is_empty())
                .then(|| parse_amount(&self.cost))
                .transpose()?,
        })
    }
}

#[derive(Default)]
pub(super) struct ManufacturingScreen {
    bill: BillForm,
    run: RunForm,
    selected_bill: Option<i64>,
    selected_run: Option<i64>,
    search: String,
}

impl App {
    pub(super) fn manufacturing_page(&mut self, ui: &mut egui::Ui, company: i64) {
        crate::theme::section(ui, "Manufacturing");
        let items = match self.store.stock_items(company) {
            Ok(items) => items,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let revisions = match self.store.material_revisions(company) {
            Ok(revisions) => revisions,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        egui::CollapsingHeader::new("Bills of materials")
            .default_open(true)
            .show(ui, |ui| {
                self.bill_form(ui, company, &items);
                self.material_history(ui, company, &revisions);
            });
        egui::CollapsingHeader::new("Production run")
            .default_open(true)
            .show(ui, |ui| {
                self.production_form(ui, company, &revisions);
            });
        egui::CollapsingHeader::new("Production history").show(ui, |ui| {
            self.production_history(ui, company);
        });
    }

    fn bill_form(&mut self, ui: &mut egui::Ui, company: i64, items: &[StockItem]) {
        ui.small("Enter the component quantities for a stated output quantity. Saving the same bill name for an item adds a version.");
        let form = &mut self.manufacturing_screen.bill;
        ui.horizontal(|ui| {
            pick_stock(
                ui,
                "bill-output-item",
                "Finished item",
                items,
                &mut form.output,
            )
        });
        egui::Grid::new("bill-fields").show(ui, |ui| {
            for (label, value) in [
                ("Bill name", &mut form.name),
                ("Output quantity", &mut form.quantity),
                ("Reason for this version", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let mut remove = None;
        for (index, component) in form.components.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                pick_stock(
                    ui,
                    ("bill-component", index),
                    "Component",
                    items,
                    &mut component.item,
                );
                ui.add(
                    egui::TextEdit::singleline(&mut component.quantity)
                        .hint_text("Required quantity")
                        .desired_width(110.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            form.components.remove(index);
        }
        ui.horizontal(|ui| {
            if ui.button("Add component").clicked() {
                form.components.push(ComponentForm::default());
            }
            if ui.button("Save bill version").clicked() {
                match form
                    .draft()
                    .and_then(|draft| self.store.save_material_revision(company, &draft))
                {
                    Ok(version) => {
                        *form = BillForm::default();
                        self.manufacturing_screen.selected_bill = Some(version);
                        self.message = format!("Bill version {version} saved");
                    }
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
    }

    fn material_history(&mut self, ui: &mut egui::Ui, company: i64, versions: &[MaterialRevision]) {
        if versions.is_empty() {
            return;
        }
        egui::CollapsingHeader::new("Bill versions").show(ui, |ui| {
            for version in versions {
                let chosen = self.manufacturing_screen.selected_bill == Some(version.id);
                if ui
                    .selectable_label(chosen, version_label(version))
                    .clicked()
                {
                    self.manufacturing_screen.selected_bill = (!chosen).then_some(version.id);
                }
                if self.manufacturing_screen.selected_bill != Some(version.id) {
                    continue;
                }
                ui.label(format!(
                    "Output {} {} | {} UTC | {}",
                    format_quantity(version.output_quantity.into()),
                    version.unit,
                    version.recorded_at,
                    version.reason
                ));
                match self
                    .store
                    .material_requirements(company, version.id, version.output_quantity)
                {
                    Ok(rows) => {
                        for row in &rows {
                            ui.label(format!(
                                "{} | {} {}",
                                row.item,
                                format_quantity(row.quantity.into()),
                                row.unit
                            ));
                        }
                        if ui.button("Copy into bill form").clicked() {
                            self.manufacturing_screen.bill = BillForm {
                                output: Some(version.output_item_id),
                                name: version.name.clone(),
                                quantity: format_quantity(version.output_quantity.into()),
                                reason: String::new(),
                                components: rows
                                    .into_iter()
                                    .map(|row| ComponentForm {
                                        item: Some(row.item_id),
                                        quantity: format_quantity(row.quantity.into()),
                                    })
                                    .collect(),
                            };
                        }
                    }
                    Err(error) => self.failure(error),
                }
            }
        });
    }

    fn production_form(&mut self, ui: &mut egui::Ui, company: i64, versions: &[MaterialRevision]) {
        if versions.is_empty() {
            ui.label("Save a bill of materials before posting a production run.");
            return;
        }
        let form = &mut self.manufacturing_screen.run;
        let previous = form.revision;
        egui::ComboBox::from_id_salt("run-bill")
            .selected_text(
                versions
                    .iter()
                    .find(|version| Some(version.id) == form.revision)
                    .map(version_label)
                    .unwrap_or_else(|| "Select bill version".into()),
            )
            .show_ui(ui, |ui| {
                for version in versions {
                    ui.selectable_value(
                        &mut form.revision,
                        Some(version.id),
                        version_label(version),
                    );
                }
            });
        if previous != form.revision {
            form.output.batch = None;
            form.requirements.clear();
        }
        ui.horizontal(|ui| {
            ui.label("Output quantity");
            if ui.text_edit_singleline(&mut form.quantity).changed() {
                form.requirements.clear();
            }
        });
        let Some(version) = versions
            .iter()
            .find(|version| Some(version.id) == form.revision)
        else {
            return;
        };
        if ui.button("Calculate required components").clicked() {
            match parse_quantity(&form.quantity).and_then(|quantity| {
                self.store
                    .material_requirements(company, version.id, quantity)
            }) {
                Ok(requirements) => {
                    form.requirements = requirements
                        .into_iter()
                        .map(|requirement| ConsumptionForm {
                            requirement,
                            bucket: BucketForm::default(),
                        })
                        .collect();
                    self.message.clear();
                }
                Err(error) => self.message = error.to_string(),
            }
        }
        if form.requirements.is_empty() {
            return;
        }
        let locations = match self.store.stock_locations(company) {
            Ok(locations) => locations,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let output_batches = match self.store.stock_batches(company, version.output_item_id) {
            Ok(batches) => batches,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let form = &mut self.manufacturing_screen.run;
        egui::Grid::new("production-run-fields").show(ui, |ui| {
            for (label, value) in [
                ("Date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Narration", &mut form.narration),
                ("Total output acquisition cost (optional)", &mut form.cost),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        default_location(&locations, &mut form.output);
        ui.horizontal_wrapped(|ui| {
            ui.label("Finished stock");
            pick_location(
                ui,
                "run-output-location",
                "Location",
                &locations,
                &mut form.output.location,
                false,
            );
            pick_batch(
                ui,
                "run-output-batch",
                &output_batches,
                &mut form.output.batch,
            );
        });
        for component in &mut form.requirements {
            let required = &component.requirement;
            let batches = match self.store.stock_batches(company, required.item_id) {
                Ok(batches) => batches,
                Err(error) => {
                    self.message = error.to_string();
                    return;
                }
            };
            default_location(&locations, &mut component.bucket);
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{} | {} {}",
                    required.item,
                    format_quantity(required.quantity.into()),
                    required.unit
                ));
                pick_location(
                    ui,
                    ("run-component-location", required.item_id),
                    "Location",
                    &locations,
                    &mut component.bucket.location,
                    false,
                );
                pick_batch(
                    ui,
                    ("run-component-batch", required.item_id),
                    &batches,
                    &mut component.bucket.batch,
                );
            });
        }
        if ui.button("Post production run").clicked() {
            match form
                .draft()
                .and_then(|draft| self.store.post_production_run(company, &draft))
            {
                Ok(run) => {
                    form.reference.clear();
                    form.narration.clear();
                    form.cost.clear();
                    form.requirements.clear();
                    self.manufacturing_screen.selected_run = Some(run);
                    self.message = format!("Production run {run} posted");
                }
                Err(error) => self.failure(error),
            }
        }
    }

    fn production_history(&mut self, ui: &mut egui::Ui, company: i64) {
        ui.add(
            egui::TextEdit::singleline(&mut self.manufacturing_screen.search)
                .hint_text("Item or reference"),
        );
        let search = self.manufacturing_screen.search.trim().to_lowercase();
        let runs = match self.store.production_runs(company) {
            Ok(runs) => runs,
            Err(error) => {
                self.failure(error);
                return;
            }
        };
        let mut found = false;
        for run in runs {
            if !search.is_empty()
                && !run.item.to_lowercase().contains(&search)
                && !run.reference.to_lowercase().contains(&search)
            {
                continue;
            }
            found = true;
            let selected = self.manufacturing_screen.selected_run == Some(run.id);
            if ui
                .selectable_label(
                    selected,
                    format!(
                        "{} | {} | {} | {} v{}",
                        run.date, run.reference, run.item, run.bill, run.revision_id
                    ),
                )
                .clicked()
            {
                self.manufacturing_screen.selected_run = (!selected).then_some(run.id);
            }
            if self.manufacturing_screen.selected_run != Some(run.id) {
                continue;
            }
            if !run.narration.is_empty() {
                ui.label(&run.narration);
            }
            match self.store.production_movements(company, run.id) {
                Ok(movements) => {
                    for movement in movements {
                        let batch = movement.batch.as_deref().unwrap_or("Unbatched");
                        ui.label(format!(
                            "{} | {} {} | {} | {batch} | Adjustment {}",
                            movement.item,
                            format_quantity(movement.quantity.into()),
                            movement.unit,
                            movement.location,
                            movement.adjustment_id
                        ));
                        if let Some(cost) = movement.acquisition_cost {
                            ui.small(format!("Acquisition cost {}", money(cost.into())));
                        }
                    }
                }
                Err(error) => self.failure(error),
            }
        }
        if !found {
            ui.label("No matching production runs.");
        }
    }
}

fn default_location(locations: &[StockLocation], form: &mut BucketForm) {
    if form.location.is_none() {
        form.location = locations
            .iter()
            .find(|location| location.name == "Main")
            .map(|location| location.id);
    }
}

fn version_label(version: &MaterialRevision) -> String {
    let suffix = if version.latest { " (latest)" } else { "" };
    format!(
        "{} | {} v{}{suffix}",
        version.item, version.name, version.id
    )
}
