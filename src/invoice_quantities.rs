//! Separate billing-only invoice allocations from physical stock movements.
use crate::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};

pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    // The line and allocation IDs are referenced by filed returns and reversals.
    // Rebuild without rewriting those references, then check them before committing.
    conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON;")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 93 {
        tx.execute_batch(
            "CREATE TABLE invoice_lines_v93 (
                id INTEGER PRIMARY KEY,
                invoice_id INTEGER NOT NULL REFERENCES invoices(id),
                item_id INTEGER NOT NULL REFERENCES stock_items(id),
                quantity INTEGER NOT NULL CHECK(quantity >= 0),
                unit_price INTEGER NOT NULL CHECK(unit_price >= 0),
                amount INTEGER NOT NULL CHECK(amount >= 0),
                location_id INTEGER REFERENCES stock_locations(id),
                batch_id INTEGER REFERENCES stock_batches(id),
                trade_ledger_id INTEGER REFERENCES ledgers(id),
                discount_kind TEXT NOT NULL DEFAULT 'None' CHECK(discount_kind IN ('None','Fixed','Percent')),
                discount_value TEXT NOT NULL DEFAULT '',
                discount_amount INTEGER NOT NULL DEFAULT 0 CHECK(discount_amount >= 0),
                rate_basis_quantity INTEGER CHECK(rate_basis_quantity > 0),
                rate_basis_unit TEXT CHECK((rate_basis_quantity IS NULL AND rate_basis_unit IS NULL)
                    OR (rate_basis_quantity IS NOT NULL AND rate_basis_unit IS NOT NULL AND length(trim(rate_basis_unit)) > 0)),
                billed_quantity INTEGER CHECK(billed_quantity >= 0),
                CHECK(quantity > 0 OR COALESCE(billed_quantity, 0) > 0)
            );
            INSERT INTO invoice_lines_v93
                SELECT id, invoice_id, item_id, quantity, unit_price, amount, location_id, batch_id,
                    trade_ledger_id, discount_kind, discount_value, discount_amount, rate_basis_quantity,
                    rate_basis_unit, billed_quantity FROM invoice_lines;
            DROP TABLE invoice_lines;
            ALTER TABLE invoice_lines_v93 RENAME TO invoice_lines;
            CREATE INDEX invoice_lines_invoice ON invoice_lines(invoice_id);",
        )?;
        for (table, owner_column, owner_table) in [
            ("invoice_line_allocations", "line_id", "invoice_lines"),
            (
                "invoice_revision_line_allocations",
                "revision_line_id",
                "invoice_revision_lines",
            ),
        ] {
            tx.execute_batch(&format!(
                "CREATE TABLE {table}_v93 (
                    id INTEGER PRIMARY KEY,
                    {owner_column} INTEGER NOT NULL REFERENCES {owner_table}(id),
                    location_id INTEGER NOT NULL REFERENCES stock_locations(id),
                    batch_id INTEGER REFERENCES stock_batches(id),
                    quantity INTEGER NOT NULL CHECK(quantity >= 0),
                    billed_quantity INTEGER CHECK(billed_quantity >= 0),
                    CHECK(quantity > 0 OR COALESCE(billed_quantity, 0) > 0)
                );
                INSERT INTO {table}_v93
                    SELECT id, {owner_column}, location_id, batch_id, quantity, billed_quantity FROM {table};
                DROP TABLE {table};
                ALTER TABLE {table}_v93 RENAME TO {table};
                CREATE INDEX {table}_line ON {table}({owner_column}, id);"
            ))?;
        }
        tx.execute_batch(
            "DROP VIEW physical_invoice_lines;
             CREATE VIEW physical_invoice_lines AS
                 SELECT l.* FROM invoice_lines l WHERE l.quantity > 0
                 AND NOT EXISTS(SELECT 1 FROM stock_note_invoices n WHERE n.invoice_id = l.invoice_id);
             DROP VIEW IF EXISTS physical_invoice_allocations;
             CREATE VIEW physical_invoice_allocations AS
                 SELECT a.* FROM invoice_line_allocations a WHERE a.quantity > 0;",
        )?;
        crate::stock_policy::refresh_stock_movements(&tx)?;
        if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(Error::Validation(
                "Invoice quantity migration found broken references".into(),
            ));
        }
        tx.pragma_update(None, "user_version", 93)?;
    }
    tx.commit()?;
    conn.execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;")?;
    Ok(())
}
