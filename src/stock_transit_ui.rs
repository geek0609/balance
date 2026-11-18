use super::App;
use balance::inventory::{format_quantity, parse_quantity};
use balance::stock_transit::{StockArrivalDraft, StockDispatch};
use balance::Result;
use eframe::egui;

#[derive(Default)]
pub(super) struct StockTransitScreen {
    pub(super) separate_dispatch: bool,
    pub(super) reference: String,
    as_of: String,
    show_completed: bool,
    arrival: Option<ArrivalForm>,
}

struct ArrivalForm {
    dispatch: i64,
    date: String,
    reference: String,
    quantity: String,
    reason: String,
    returned: bool,
}

impl App {
    pub(super) fn stock_transit_view(&mut self, ui: &mut egui::Ui, company: i64) -> Result<()> {
        ui.horizontal(|ui| {
            ui.label("As of (optional)");
            ui.add(
                egui::TextEdit::singleline(&mut self.stock_transit_screen.as_of)
                    .hint_text("YYYY-MM-DD"),
            );
            ui.checkbox(
                &mut self.stock_transit_screen.show_completed,
                "Include completed",
            );
        });
        let raw_date = self.stock_transit_screen.as_of.trim().to_owned();
        let as_of = (!raw_date.is_empty()).then_some(raw_date.as_str());
        self.report_export_path_field(ui);
        if ui.button("Export transfers as CSV").clicked() {
            self.store.export_stock_dispatches_csv(
                company,
                as_of,
                self.stock_transit_screen.show_completed,
                self.report_export_path.trim(),
            )?;
            self.message = "Stock transfers exported".into();
        }
        let rows = self.store.stock_dispatches(company, as_of)?;
        let mut shown = false;
        for row in rows {
            if row.in_transit() == 0 && !self.stock_transit_screen.show_completed {
                continue;
            }
            shown = true;
            egui::CollapsingHeader::new(format!(
                "{} · {} · {} → {} · {} {} in transit",
                row.reference,
                row.item,
                row.from,
                row.to,
                format_quantity(row.in_transit().into()),
                row.unit
            ))
            .id_salt(("stock-dispatch", row.id))
            .show(ui, |ui| -> Result<()> {
                ui.label(format!(
                    "{} · {} {} dispatched · {} · {}",
                    row.date,
                    format_quantity(row.quantity.into()),
                    row.unit,
                    row.batch.as_deref().unwrap_or("Unbatched"),
                    row.reason,
                ));
                for arrival in self.store.stock_arrivals(company, row.id, as_of)? {
                    ui.label(format!(
                        "{} · {} · {} {} {} · {}",
                        arrival.date,
                        arrival.reference,
                        format_quantity(arrival.quantity.into()),
                        row.unit,
                        if arrival.returned {
                            "returned to source"
                        } else {
                            "received"
                        },
                        arrival.reason,
                    ));
                }
                if as_of.is_none() && row.in_transit() > 0 {
                    self.stock_arrival_form(ui, company, &row)?;
                }
                Ok(())
            })
            .body_returned
            .transpose()?;
        }
        if !shown {
            ui.label("No matching transfers.");
        }
        Ok(())
    }

    fn stock_arrival_form(
        &mut self,
        ui: &mut egui::Ui,
        company: i64,
        row: &StockDispatch,
    ) -> Result<()> {
        let editing = self
            .stock_transit_screen
            .arrival
            .as_ref()
            .is_some_and(|form| form.dispatch == row.id);
        if !editing && ui.button("Record receipt or return").clicked() {
            self.stock_transit_screen.arrival = Some(ArrivalForm {
                dispatch: row.id,
                date: String::new(),
                reference: String::new(),
                quantity: format_quantity(row.in_transit().into()),
                reason: String::new(),
                returned: false,
            });
        }
        let Some(form) = self
            .stock_transit_screen
            .arrival
            .as_mut()
            .filter(|form| form.dispatch == row.id)
        else {
            return Ok(());
        };
        ui.checkbox(&mut form.returned, "Return to source");
        egui::Grid::new(("stock-arrival", row.id)).show(ui, |ui| {
            for (label, value) in [
                ("Date (YYYY-MM-DD)", &mut form.date),
                ("Reference", &mut form.reference),
                ("Quantity", &mut form.quantity),
                ("Reason", &mut form.reason),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
                ui.end_row();
            }
        });
        let (save, cancel) = ui
            .horizontal(|ui| {
                (
                    ui.button(if form.returned {
                        "Save return"
                    } else {
                        "Save receipt"
                    })
                    .clicked(),
                    ui.button("Cancel").clicked(),
                )
            })
            .inner;
        if save {
            self.store.record_stock_arrival(
                company,
                row.id,
                &StockArrivalDraft {
                    date: form.date.clone(),
                    reference: form.reference.clone(),
                    quantity: parse_quantity(&form.quantity)?,
                    reason: form.reason.clone(),
                    returned: form.returned,
                },
            )?;
            self.message = if form.returned {
                "Stock returned"
            } else {
                "Stock received"
            }
            .into();
            self.stock_transit_screen.arrival = None;
        } else if cancel {
            self.stock_transit_screen.arrival = None;
        }
        Ok(())
    }
}
