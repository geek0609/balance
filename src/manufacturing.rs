//! Bills of materials and quantity-only production runs.
use crate::inventory::{check_quantity_precision, check_stock_batch, stock_bucket_quantity};
use crate::{parse_date, required, Error, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{HashMap, HashSet};

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE bills_of_materials(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            name TEXT NOT NULL,
            name_key TEXT NOT NULL,
            UNIQUE(company_id, item_id, name_key)
        );
        CREATE TABLE material_revisions(
            id INTEGER PRIMARY KEY,
            bill_id INTEGER NOT NULL REFERENCES bills_of_materials(id),
            output_quantity INTEGER NOT NULL CHECK(output_quantity > 0),
            reason TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX material_revisions_bill ON material_revisions(bill_id, id);
        CREATE TABLE material_components(
            revision_id INTEGER NOT NULL REFERENCES material_revisions(id),
            item_id INTEGER NOT NULL REFERENCES stock_items(id),
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            PRIMARY KEY(revision_id, item_id)
        );
        CREATE TABLE production_runs(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            revision_id INTEGER NOT NULL REFERENCES material_revisions(id),
            date TEXT NOT NULL,
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            narration TEXT NOT NULL,
            UNIQUE(company_id, reference_key)
        );
        CREATE INDEX production_runs_company ON production_runs(company_id, date, id);
        CREATE TABLE production_movements(
            run_id INTEGER NOT NULL REFERENCES production_runs(id),
            adjustment_id INTEGER NOT NULL UNIQUE REFERENCES stock_adjustments(id),
            PRIMARY KEY(run_id, adjustment_id)
        );",
    )?;
    crate::periods::protect_dated_table(conn, "production_runs")
}

#[derive(Debug, Clone)]
pub struct ComponentDraft {
    pub item_id: i64,
    pub quantity: i64,
}

#[derive(Debug, Clone)]
pub struct MaterialDraft {
    pub output_item_id: i64,
    pub name: String,
    pub output_quantity: i64,
    pub reason: String,
    pub components: Vec<ComponentDraft>,
}

#[derive(Debug, Clone)]
pub struct MaterialRevision {
    pub id: i64,
    pub bill_id: i64,
    pub name: String,
    pub output_item_id: i64,
    pub item: String,
    pub unit: String,
    pub output_quantity: i64,
    pub reason: String,
    pub recorded_at: String,
    pub latest: bool,
}

#[derive(Debug, Clone)]
pub struct ComponentRequirement {
    pub item_id: i64,
    pub item: String,
    pub unit: String,
    pub quantity: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct StockBucket {
    pub location_id: i64,
    pub batch_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ProductionDraft {
    pub revision_id: i64,
    pub date: String,
    pub reference: String,
    pub narration: String,
    pub quantity: i64,
    pub output: StockBucket,
    pub components: HashMap<i64, StockBucket>,
    pub acquisition_cost: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ProductionRun {
    pub id: i64,
    pub revision_id: i64,
    pub bill: String,
    pub item: String,
    pub date: String,
    pub reference: String,
    pub narration: String,
}

#[derive(Debug, Clone)]
pub struct ProductionMovement {
    pub adjustment_id: i64,
    pub item: String,
    pub unit: String,
    pub location: String,
    pub batch: Option<String>,
    pub quantity: i64,
    pub acquisition_cost: Option<i64>,
}

const REVISION_SQL: &str =
    "SELECT r.id, b.id, b.name, s.id, s.name, s.unit, r.output_quantity, r.reason, r.recorded_at,
            NOT EXISTS(SELECT 1 FROM material_revisions later WHERE later.bill_id = b.id AND later.id > r.id)
     FROM material_revisions r JOIN bills_of_materials b ON b.id = r.bill_id
     JOIN stock_items s ON s.id = b.item_id AND s.company_id = b.company_id
     WHERE b.company_id = ?1";

fn read_revision(row: &rusqlite::Row<'_>) -> rusqlite::Result<MaterialRevision> {
    Ok(MaterialRevision {
        id: row.get(0)?,
        bill_id: row.get(1)?,
        name: row.get(2)?,
        output_item_id: row.get(3)?,
        item: row.get(4)?,
        unit: row.get(5)?,
        output_quantity: row.get(6)?,
        reason: row.get(7)?,
        recorded_at: row.get(8)?,
        latest: row.get(9)?,
    })
}

fn revision(conn: &Connection, company: i64, id: i64) -> Result<MaterialRevision> {
    conn.query_row(
        &format!("{REVISION_SQL} AND r.id = ?2"),
        params![company, id],
        read_revision,
    )
    .optional()?
    .ok_or_else(|| Error::Validation("Bill of materials version not found in this company".into()))
}

fn requirements(
    conn: &Connection,
    company: i64,
    revision: &MaterialRevision,
    quantity: i64,
) -> Result<Vec<ComponentRequirement>> {
    if quantity <= 0 {
        return Err(Error::Validation(
            "Enter a positive production quantity".into(),
        ));
    }
    check_quantity_precision(conn, company, revision.output_item_id, quantity)?;
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.unit, c.quantity FROM material_components c JOIN stock_items s ON s.id = c.item_id
         WHERE c.revision_id = ?1 AND s.company_id = ?2 ORDER BY s.name, s.id",
    )?;
    let mut rows = stmt.query(params![revision.id, company])?;
    let mut requirements = Vec::new();
    let divisor = i128::from(revision.output_quantity);
    while let Some(row) = rows.next()? {
        let item_id = row.get(0)?;
        let item: String = row.get(1)?;
        let base = row.get::<_, i64>(3)?;
        let scaled = i128::from(base) * i128::from(quantity);
        if scaled % divisor != 0 {
            return Err(Error::Validation(format!(
                "Production quantity needs an inexact quantity of {item}"
            )));
        }
        let quantity = i64::try_from(scaled / divisor)
            .map_err(|_| Error::Validation(format!("Required quantity of {item} is too large")))?;
        check_quantity_precision(conn, company, item_id, quantity)
            .map_err(|error| Error::Validation(format!("Required quantity of {item}: {error}")))?;
        requirements.push(ComponentRequirement {
            item_id,
            item,
            unit: row.get(2)?,
            quantity,
        });
    }
    if requirements.is_empty() {
        return Err(Error::Validation(
            "This bill of materials has no components".into(),
        ));
    }
    Ok(requirements)
}

fn check_bucket(conn: &Connection, company: i64, item: i64, bucket: StockBucket) -> Result<()> {
    let location_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stock_locations WHERE company_id = ?1 AND id = ?2)",
        params![company, bucket.location_id],
        |row| row.get(0),
    )?;
    if !location_exists {
        return Err(Error::Validation(
            "Stock location not found in this company".into(),
        ));
    }
    check_stock_batch(conn, company, item, bucket.batch_id)
}

fn insert_movement(
    conn: &Connection,
    company: i64,
    run: i64,
    item: i64,
    date: &str,
    quantity: i64,
    bucket: StockBucket,
    reason: &str,
) -> Result<i64> {
    let adjustment = crate::inventory::record_stock_adjustment(
        conn,
        company,
        item,
        bucket.location_id,
        bucket.batch_id,
        date,
        quantity,
        reason,
    )?;
    conn.execute(
        "INSERT INTO production_movements(run_id, adjustment_id) VALUES (?1, ?2)",
        params![run, adjustment],
    )?;
    Ok(adjustment)
}

impl Store {
    /// Record a new version of a named bill for a finished item.
    pub fn save_material_revision(&mut self, company: i64, draft: &MaterialDraft) -> Result<i64> {
        let name = required(&draft.name, "Bill of materials name")?;
        let reason = required(&draft.reason, "Reason for saving this version")?;
        if draft.output_quantity <= 0 || draft.components.is_empty() {
            return Err(Error::Validation(
                "Enter a positive output quantity and at least one component".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_quantity_precision(&tx, company, draft.output_item_id, draft.output_quantity)?;
        let mut items = HashSet::new();
        for component in &draft.components {
            if component.item_id == draft.output_item_id {
                return Err(Error::Validation(
                    "The finished item cannot be its own component".into(),
                ));
            }
            if component.quantity <= 0 || !items.insert(component.item_id) {
                return Err(Error::Validation(
                    "Enter each component once with a positive quantity".into(),
                ));
            }
            check_quantity_precision(&tx, company, component.item_id, component.quantity)?;
        }
        let key = name.to_lowercase();
        tx.execute(
            "INSERT INTO bills_of_materials(company_id, item_id, name, name_key) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(company_id, item_id, name_key) DO NOTHING",
            params![company, draft.output_item_id, name, key],
        )?;
        let bill: i64 = tx.query_row(
            "SELECT id FROM bills_of_materials WHERE company_id = ?1 AND item_id = ?2 AND name_key = ?3",
            params![company, draft.output_item_id, key], |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO material_revisions(bill_id, output_quantity, reason) VALUES (?1, ?2, ?3)",
            params![bill, draft.output_quantity, reason],
        )?;
        let version = tx.last_insert_rowid();
        for component in &draft.components {
            tx.execute(
                "INSERT INTO material_components(revision_id, item_id, quantity) VALUES (?1, ?2, ?3)",
                params![version, component.item_id, component.quantity],
            )?;
        }
        tx.commit()?;
        Ok(version)
    }

    pub fn material_revisions(&self, company: i64) -> Result<Vec<MaterialRevision>> {
        let mut stmt = self.conn.prepare(&format!(
            "{REVISION_SQL} ORDER BY s.name, b.name, r.id DESC"
        ))?;
        let rows = stmt.query_map([company], read_revision)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn material_requirements(
        &self,
        company: i64,
        revision_id: i64,
        quantity: i64,
    ) -> Result<Vec<ComponentRequirement>> {
        let revision = revision(&self.conn, company, revision_id)?;
        requirements(&self.conn, company, &revision, quantity)
    }

    pub fn post_production_run(&mut self, company: i64, draft: &ProductionDraft) -> Result<i64> {
        let date = parse_date(&draft.date)?;
        let reference = required(&draft.reference, "Production reference")?;
        if draft.acquisition_cost.is_some_and(|cost| cost < 0) {
            return Err(Error::Validation(
                "Acquisition cost cannot be negative".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision = revision(&tx, company, draft.revision_id)?;
        let requirements = requirements(&tx, company, &revision, draft.quantity)?;
        if draft.components.len() != requirements.len() {
            return Err(Error::Validation(
                "Choose a stock bucket for each component".into(),
            ));
        }
        check_bucket(&tx, company, revision.output_item_id, draft.output)?;
        for component in &requirements {
            let bucket = draft.components.get(&component.item_id).ok_or_else(|| {
                Error::Validation(format!("Select location and batch for {}", component.item))
            })?;
            check_bucket(&tx, company, component.item_id, *bucket)?;
            let available = stock_bucket_quantity(
                &tx,
                company,
                component.item_id,
                bucket.batch_id,
                Some(bucket.location_id),
                Some(date),
            )?;
            if available < i128::from(component.quantity) {
                return Err(Error::Validation(format!(
                    "{} has insufficient stock in the selected location and batch",
                    component.item
                )));
            }
        }
        let date = date.format("%Y-%m-%d").to_string();
        let inserted = tx.execute(
            "INSERT INTO production_runs(company_id, revision_id, date, reference, reference_key, narration)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(company_id, reference_key) DO NOTHING",
            params![company, revision.id, date, reference, reference.to_lowercase(), draft.narration.trim()],
        )?;
        if inserted == 0 {
            return Err(Error::Validation(
                "This production reference already exists".into(),
            ));
        }
        let run = tx.last_insert_rowid();
        let reason = format!("Production {reference}");
        for component in requirements {
            insert_movement(
                &tx,
                company,
                run,
                component.item_id,
                &date,
                -component.quantity,
                draft.components[&component.item_id],
                &reason,
            )?;
        }
        let output = insert_movement(
            &tx,
            company,
            run,
            revision.output_item_id,
            &date,
            draft.quantity,
            draft.output,
            &reason,
        )?;
        if let Some(amount) = draft.acquisition_cost {
            crate::actual_cost::insert_receipt_cost(
                &tx,
                output,
                amount,
                "Production cost entered with the run",
            )?;
        }
        tx.commit()?;
        Ok(run)
    }

    pub fn production_runs(&self, company: i64) -> Result<Vec<ProductionRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, r.id, b.name, i.name, p.date, p.reference, p.narration FROM production_runs p
             JOIN material_revisions r ON r.id = p.revision_id JOIN bills_of_materials b ON b.id = r.bill_id
             JOIN stock_items i ON i.id = b.item_id
             WHERE p.company_id = ?1 ORDER BY p.date DESC, p.id DESC",
        )?;
        let rows = stmt.query_map([company], |row| {
            Ok(ProductionRun {
                id: row.get(0)?,
                revision_id: row.get(1)?,
                bill: row.get(2)?,
                item: row.get(3)?,
                date: row.get(4)?,
                reference: row.get(5)?,
                narration: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn production_movements(&self, company: i64, run: i64) -> Result<Vec<ProductionMovement>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, i.name, i.unit, l.name, b.name, a.quantity, (
                SELECT c.amount FROM stock_receipt_costs c WHERE c.adjustment_id = a.id ORDER BY c.id DESC LIMIT 1
             )
             FROM production_movements m JOIN production_runs p ON p.id = m.run_id
             JOIN stock_adjustments a ON a.id = m.adjustment_id
             JOIN stock_items i ON i.id = a.item_id JOIN stock_locations l ON l.id = a.location_id
             LEFT JOIN stock_batches b ON b.id = a.batch_id WHERE p.company_id = ?1 AND p.id = ?2
             ORDER BY a.quantity DESC, a.id",
        )?;
        let rows = stmt.query_map(params![company, run], |row| {
            Ok(ProductionMovement {
                adjustment_id: row.get(0)?,
                item: row.get(1)?,
                unit: row.get(2)?,
                location: row.get(3)?,
                batch: row.get(4)?,
                quantity: row.get(5)?,
                acquisition_cost: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
