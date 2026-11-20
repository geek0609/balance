//! Stock-only XML documents and matching source tracking references.
use super::*;
use crate::invoice_stock_links::StockNoteInvoiceAllocation;
use crate::stock_notes::{InvoiceQuantityShare, StockNoteDraft, StockNoteLineDraft};

#[derive(Debug, Clone)]
pub(super) struct ImportedStockNote {
    pub date: String,
    pub reference: String,
    pub party: String,
    pub kind: InvoiceKind,
    party_id: Option<i64>,
    narration: String,
    lines: Vec<NoteLine>,
    metadata: SourceMetadata,
}

#[derive(Debug, Clone)]
struct NoteLine {
    item: String,
    item_id: Option<i64>,
    unit: String,
    precision: u8,
    allocation: ImportedStockAllocation,
    value: Option<i64>,
    order: orders::OrderFields,
    trade_default: Option<trade_defaults::TradeDefault>,
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS imported_invoice_tracking(
            allocation_id INTEGER PRIMARY KEY REFERENCES invoice_line_allocations(id),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            reported_stock_quantity INTEGER NOT NULL CHECK(reported_stock_quantity >= 0)
         );
         CREATE INDEX IF NOT EXISTS imported_invoice_tracking_reference ON imported_invoice_tracking(reference_key);
         CREATE TABLE IF NOT EXISTS imported_stock_note_tracking(
            note_line_id INTEGER PRIMARY KEY REFERENCES stock_note_lines(id),
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS imported_stock_note_tracking_reference ON imported_stock_note_tracking(reference_key);",
    )?;
    Ok(())
}

pub(super) fn kind(value: &str) -> Option<InvoiceKind> {
    match normalized(value).as_str() {
        "delivery note" => Some(InvoiceKind::Sale),
        "receipt note" => Some(InvoiceKind::Purchase),
        _ => None,
    }
}

pub(super) fn tracking(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("Not Applicable"))
}

fn stored_value(node: Node<'_, '_>) -> Result<Option<i64>> {
    direct_text(node, "AMOUNT")
        .filter(|value| !value.is_empty())
        .map(|value| {
            xml_amount(&value)?
                .checked_abs()
                .ok_or_else(|| Error::Validation("Stock note value is too large".into()))
        })
        .transpose()
}

struct NoteStock<'a> {
    kind: InvoiceKind,
    items: &'a InvoiceStockItems,
    locations: &'a HashMap<String, Option<i64>>,
    batches: &'a HashMap<(String, String), i64>,
    order: orders::OrderFields,
    accounts: trade_defaults::SourceAccounts<'a>,
    common_ledger: Option<trade_defaults::TradeDefault>,
}

impl NoteStock<'_> {
    fn allocations(
        &self,
        node: Node<'_, '_>,
        label: &str,
        issues: &mut Vec<String>,
    ) -> Result<Vec<NoteLine>> {
        invoice_list_fields(
            node,
            &[
                "STOCKITEMNAME",
                "ACTUALQTY",
                "BILLEDQTY",
                "RATE",
                "AMOUNT",
                "ISDEEMEDPOSITIVE",
                "GODOWNNAME",
                "TRACKINGNUMBER",
                "BASICORDERREF",
                "ORDERNO",
                "ORDERDUEDATE",
                "BATCHALLOCATIONS.LIST",
                "ACCOUNTINGALLOCATIONS.LIST",
            ],
            label,
            issues,
        );
        let item = invoice_field(node, "STOCKITEMNAME")?;
        let (item_id, unit) = self
            .items
            .get(&normalized(&item))
            .ok_or_else(|| Error::Validation(format!("Stock item {item} is unavailable")))?;
        let actual = unit.quantity(&invoice_field(node, "ACTUALQTY")?)?;
        if actual == 0 || (self.kind == InvoiceKind::Purchase && actual < 0) {
            return Err(Error::Validation(
                "Invalid physical stock direction on stock note".into(),
            ));
        }
        let quantity = actual
            .checked_abs()
            .ok_or_else(|| Error::Validation("Stock quantity is too large".into()))?;
        let header_tracking = direct_text(node, "TRACKINGNUMBER");
        let header_location = direct_text(node, "GODOWNNAME").filter(|value| !value.is_empty());
        let batches: Vec<_> = node
            .children()
            .filter(|entry| {
                entry.is_element()
                    && local_name(*entry).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
                    && meaningful(*entry)
            })
            .collect();
        let rows = if batches.is_empty() {
            vec![node]
        } else {
            batches
        };
        let item_order = orders::OrderFields::read(node, &self.order)?;
        let trade_default = self
            .accounts
            .item(node, self.common_ledger.as_ref(), issues)?;
        let mut planned = Vec::new();
        for entry in rows {
            if entry != node {
                invoice_list_fields(
                    entry,
                    &[
                        "GODOWNNAME",
                        "DESTINATIONGODOWNNAME",
                        "BATCHNAME",
                        "ACTUALQTY",
                        "BILLEDQTY",
                        "AMOUNT",
                        "TRACKINGNUMBER",
                        "BASICORDERREF",
                        "ORDERNO",
                        "ORDERDUEDATE",
                        "ACCOUNTINGALLOCATIONS.LIST",
                    ],
                    label,
                    issues,
                );
            }
            let order = if entry == node {
                item_order.clone()
            } else {
                orders::OrderFields::read(entry, &item_order)?
            };
            order.validate_allocation()?;
            let allocation_default = if entry == node {
                trade_default.clone()
            } else {
                self.accounts.item(entry, trade_default.as_ref(), issues)?
            };
            let location = direct_text(entry, "GODOWNNAME")
                .filter(|value| !value.is_empty())
                .or_else(|| header_location.clone())
                .unwrap_or_else(|| "Main".into());
            if direct_text(entry, "DESTINATIONGODOWNNAME").is_some_and(|value| {
                !value.is_empty() && normalized(&value) != normalized(&location)
            }) {
                return Err(Error::Validation(
                    "Stock note source and destination locations differ".into(),
                ));
            }
            let location_id = *self.locations.get(&normalized(&location)).ok_or_else(|| {
                Error::Validation(format!("Stock location {location} is unavailable"))
            })?;
            let part_actual = unit.quantity(&invoice_field(entry, "ACTUALQTY")?)?;
            if part_actual.signum() != actual.signum() {
                return Err(Error::Validation(
                    "Stock note batch quantity has the wrong direction".into(),
                ));
            }
            let part_quantity = part_actual
                .checked_abs()
                .ok_or_else(|| Error::Validation("Stock quantity is too large".into()))?;
            let step = 10_i64.pow(u32::from(3 - unit.precision));
            if part_quantity % step != 0 {
                return Err(Error::Validation(
                    "Stock note quantity exceeds unit precision".into(),
                ));
            }
            let batch = direct_text(entry, "BATCHNAME").filter(|value| !value.is_empty());
            let batch_id = batch.as_ref().and_then(|name| {
                self.batches
                    .get(&(normalized(&item), normalized(name)))
                    .copied()
            });
            planned.push(NoteLine {
                item: item.clone(),
                item_id: *item_id,
                unit: unit.symbol.clone(),
                precision: unit.precision,
                allocation: ImportedStockAllocation {
                    location,
                    existing_location_id: location_id,
                    batch,
                    existing_batch_id: batch_id,
                    quantity: part_quantity,
                    billed_quantity: 0,
                    tracking: tracking(
                        direct_text(entry, "TRACKINGNUMBER").or_else(|| header_tracking.clone()),
                    ),
                },
                value: stored_value(entry)?,
                order,
                trade_default: allocation_default,
            });
        }
        if planned
            .iter()
            .map(|line| i128::from(line.allocation.quantity))
            .sum::<i128>()
            != i128::from(quantity)
        {
            return Err(Error::Validation(
                "Stock note batch quantities do not match the item quantity".into(),
            ));
        }
        let total_value = stored_value(node)?;
        let rate_value = direct_text(node, "RATE")
            .filter(|rate| !rate.is_empty())
            .map(|rate| {
                let (price, suffix) = rate.split_once('/').ok_or_else(|| {
                    Error::Validation("Stock note rate must specify a unit".into())
                })?;
                discounted_line_amount_with_basis(
                    quantity,
                    parse_amount(price)?,
                    &InvoiceDiscount::None,
                    unit.rate_basis(suffix)?.as_ref(),
                )
                .map(|(amount, _)| amount)
            })
            .transpose()?;
        match (
            total_value.or(rate_value),
            planned.iter().filter(|line| line.value.is_some()).count(),
        ) {
            (Some(total), 0) => {
                let mut offset = 0;
                for line in &mut planned {
                    let qty = line.allocation.quantity;
                    let share = InvoiceQuantityShare::for_range(quantity, offset, qty)?;
                    line.value = Some(share.apportion(i128::from(total)) as i64);
                    offset += qty;
                }
            }
            (Some(total), known) if known == planned.len() => {
                if planned
                    .iter()
                    .map(|line| i128::from(line.value.unwrap_or_default()))
                    .sum::<i128>()
                    != i128::from(total)
                {
                    return Err(Error::Validation(
                        "Stock note batch values do not match the item amount".into(),
                    ));
                }
            }
            (Some(_), _) => {
                return Err(Error::Validation(
                    "Stock note batch values are incomplete".into(),
                ))
            }
            (None, _) => {}
        }
        Ok(planned)
    }
}

pub(super) fn parse(
    node: Node<'_, '_>,
    kind: InvoiceKind,
    label: &str,
    metadata: SourceMetadata,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    items: &InvoiceStockItems,
    locations: &HashMap<String, Option<i64>>,
    batches: &HashMap<(String, String), i64>,
    issues: &mut Vec<String>,
) -> Result<ImportedStockNote> {
    check_fields(
        node,
        &[
            "DATE",
            "VOUCHERTYPENAME",
            "VOUCHERNUMBER",
            "PARTYLEDGERNAME",
            "NARRATION",
            "PERSISTEDVIEW",
            "OBJVIEW",
            "ISINVOICE",
            "ALLINVENTORYENTRIES.LIST",
            "INVENTORYENTRIES.LIST",
            "LEDGERENTRIES.LIST",
            "ALLLEDGERENTRIES.LIST",
            "ISCANCELLED",
            "ISOPTIONAL",
            "ORDERNO",
            "BASICORDERREF",
            "ORDERDUEDATE",
        ],
        label,
        issues,
    );
    check_voucher_view(node, &["Inventory Voucher View", "Invoice Voucher View"])?;
    for flag in ["ISCANCELLED", "ISOPTIONAL"] {
        if direct_text(node, flag)
            .is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("no"))
        {
            return Err(Error::Validation(
                "Cancelled and optional stock notes cannot be posted".into(),
            ));
        }
    }
    let party = invoice_field(node, "PARTYLEDGERNAME")?;
    let (party_id, category) = ledgers
        .get(&normalized(&party))
        .ok_or_else(|| Error::Validation(format!("Party {party} is unavailable")))?;
    if !matches!(category, Category::Asset | Category::Liability) {
        return Err(Error::Validation(
            "Stock note party is not an asset or liability ledger".into(),
        ));
    }
    let accounts = trade_defaults::SourceAccounts {
        kind,
        party: &party,
        ledgers,
        label,
        strict: true,
    };
    let voucher_defaults = accounts.voucher(node, issues)?;
    let stock = NoteStock {
        kind,
        items,
        locations,
        batches,
        order: orders::OrderFields::read(node, &orders::OrderFields::default())?,
        accounts,
        common_ledger: voucher_defaults.common,
    };
    let mut lines = Vec::new();
    for entry in node
        .children()
        .filter(|entry| entry.is_element() && meaningful(*entry))
    {
        let entry_type = local_name(entry).to_ascii_uppercase();
        if matches!(
            entry_type.as_str(),
            "ALLINVENTORYENTRIES.LIST" | "INVENTORYENTRIES.LIST"
        ) {
            lines.extend(stock.allocations(entry, label, issues)?);
        }
    }
    if lines.is_empty() {
        return Err(Error::Validation(
            "Stock note needs at least one inventory line".into(),
        ));
    }
    if voucher_defaults.requires_line_accounts
        && lines.iter().any(|line| line.trade_default.is_none())
    {
        return Err(Error::Validation(
            "Stock note ledger amounts require a clear trade ledger for each stock allocation"
                .into(),
        ));
    }
    if voucher_defaults.valued_accounts.iter().any(|account| {
        !lines.iter().any(|line| {
            line.trade_default
                .as_ref()
                .is_some_and(|trade| trade.same_ledger(account))
        })
    }) {
        return Err(Error::Validation(
            "Stock note has a ledger amount with no corresponding stock allocation".into(),
        ));
    }
    Ok(ImportedStockNote {
        date: xml_date(&invoice_field(node, "DATE")?)?,
        reference: invoice_field(node, "VOUCHERNUMBER")?,
        party,
        kind,
        party_id: *party_id,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
        lines,
        metadata,
    })
}

#[derive(Default)]
pub(super) struct NewTracking {
    references: HashSet<String>,
    invoices: HashSet<i64>,
    notes: HashSet<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrackingMatch {
    note_line_id: i64,
    invoice_allocation_id: i64,
    quantity: i64,
}

impl NewTracking {
    pub(super) fn write_note(
        &mut self,
        conn: &Connection,
        company: i64,
        note: &ImportedStockNote,
        created_batches: &mut HashMap<(i64, String), i64>,
    ) -> Result<()> {
        let party = find_ledger(conn, company, &note.party)?
            .ok_or_else(|| Error::Validation(format!("Party {} is missing", note.party)))?;
        if note.party_id.is_some_and(|id| id != party) {
            return Err(Error::Validation(format!(
                "Party {} changed since preview",
                note.party
            )));
        }
        let mut ordered = Vec::with_capacity(note.lines.len());
        let mut order_ids = HashMap::new();
        let mut lines = Vec::with_capacity(note.lines.len());
        let mut costs = Vec::with_capacity(note.lines.len());
        for line in &note.lines {
            let source = if let Some(reference) = &line.order.reference {
                let key = normalized(reference);
                let order_id = match order_ids.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => *entry.insert(
                        orders::linked_order(conn, company, party, note.kind, reference)?,
                    ),
                };
                Some(crate::orders::OrderAllocation {
                    order_id,
                    expected_on: line.order.expected_on.as_deref(),
                })
            } else {
                None
            };
            ordered.push(source);
            let item_id = resolve_imported_item(
                conn,
                company,
                &line.item,
                line.item_id,
                &line.unit,
                line.precision,
            )?;
            let (location_id, batch_id) = resolve_imported_allocation(
                conn,
                company,
                item_id,
                &line.allocation,
                created_batches,
            )?;
            let quantity = line.allocation.quantity;
            let price = line.value.map_or(0, |value| {
                (i128::from(value) * 1000 + i128::from(quantity / 2)) / i128::from(quantity)
            });
            lines.push(StockNoteLineDraft {
                item_id,
                location_id,
                batch_id,
                quantity,
                unit_price: i64::try_from(price)
                    .map_err(|_| Error::Validation("Stock note rate is too large".into()))?,
            });
            costs.push(line.value);
        }
        let order_id = ordered
            .first()
            .copied()
            .flatten()
            .map(|source| source.order_id)
            .filter(|&id| {
                ordered
                    .iter()
                    .all(|source| source.is_some_and(|source| source.order_id == id))
            });
        let draft = StockNoteDraft {
            kind: note.kind,
            date: note.date.clone(),
            reference: note.reference.clone(),
            party_ledger_id: party,
            order_id,
            narration: note.narration.clone(),
            lines,
        };
        let id =
            crate::stock_notes::write_stock_note(conn, company, &draft, Some(&costs), &ordered)?;
        save_metadata(conn, company, "stocknote", id, &note.metadata)?;
        let mut stmt =
            conn.prepare("SELECT id FROM stock_note_lines WHERE note_id = ?1 ORDER BY id")?;
        let ids = stmt
            .query_map([id], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.len() != note.lines.len() {
            return Err(Error::Validation(
                "Stock note allocations are inconsistent".into(),
            ));
        }
        for (id, line) in ids.into_iter().zip(&note.lines) {
            if let Some(account) = &line.trade_default {
                let ledger = account.resolve(conn, company, party, note.kind)?;
                conn.execute(
                    "INSERT INTO stock_note_trade_defaults(note_line_id, ledger_id)
                     VALUES (?1, ?2) ON CONFLICT(note_line_id) DO UPDATE SET ledger_id = excluded.ledger_id",
                    params![id, ledger],
                )?;
            }
            if let Some(reference) = &line.allocation.tracking {
                let key = normalized(reference);
                conn.execute(
                    "INSERT INTO imported_stock_note_tracking VALUES (?1, ?2, ?3)",
                    params![id, reference, key],
                )?;
                self.references.insert(key);
                self.notes.insert(id);
            }
        }
        Ok(())
    }

    pub(super) fn write_invoice(
        &mut self,
        conn: &Connection,
        invoice: i64,
        source: &ImportedInvoice,
    ) -> Result<()> {
        let all_allocations = source.lines.iter().flat_map(|line| &line.allocations);
        if !all_allocations
            .clone()
            .any(|allocation| allocation.tracking.is_some())
        {
            return Ok(());
        }
        let mut stmt = conn.prepare(
            "SELECT a.id FROM invoice_line_allocations a JOIN invoice_lines l ON l.id = a.line_id
             WHERE l.invoice_id = ?1 ORDER BY l.id, a.id",
        )?;
        let ids = stmt
            .query_map([invoice], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.len() != all_allocations.clone().count() {
            return Err(Error::Validation(
                "Tracked invoice allocations are inconsistent".into(),
            ));
        }
        for (id, allocation) in ids.into_iter().zip(all_allocations) {
            if let Some(reference) = &allocation.tracking {
                let key = normalized(reference);
                conn.execute(
                    "INSERT INTO imported_invoice_tracking VALUES (?1, ?2, ?3, ?4)",
                    params![id, reference, key, allocation.quantity],
                )?;
                self.references.insert(key);
                self.invoices.insert(id);
            }
        }
        Ok(())
    }

    /// Match documents from this batch against the same tracking references already on file.
    pub(super) fn match_documents(
        self,
        conn: &Connection,
        company: i64,
    ) -> Result<Vec<TrackingMatch>> {
        let mut notes = conn.prepare(
            "SELECT n.id, l.id, n.date, l.quantity - COALESCE(b.billed, 0) - COALESCE(r.returned, 0)
             FROM imported_stock_note_tracking t JOIN stock_note_lines l ON l.id = t.note_line_id
             JOIN stock_notes n ON n.id = l.note_id
             LEFT JOIN stock_note_billed_quantities b ON b.note_line_id = l.id
             LEFT JOIN stock_note_returned_quantities r ON r.note_line_id = l.id
             WHERE n.company_id = ?1 AND n.reversed_on IS NULL AND t.reference_key = ?2
             ORDER BY n.date, n.id, l.id",
        )?;
        let mut demands = conn.prepare(
            "SELECT i.id, a.id, v.date, a.billed_quantity - COALESCE((SELECT SUM(s.quantity)
                  FROM active_invoice_stock_links s WHERE s.invoice_allocation_id = a.id), 0), dates.expected_on
             FROM stock_note_lines nl JOIN stock_notes n ON n.id = nl.note_id
             JOIN imported_stock_note_tracking nt ON nt.note_line_id = nl.id
             JOIN imported_invoice_tracking it ON it.reference_key = nt.reference_key
             JOIN invoice_line_allocations a ON a.id = it.allocation_id AND a.location_id = nl.location_id
                  AND (a.batch_id IS NULL OR a.batch_id = nl.batch_id)
             JOIN invoice_lines l ON l.id = a.line_id AND l.item_id = nl.item_id
             JOIN invoices i ON i.id = l.invoice_id AND i.company_id = n.company_id
                  AND i.kind = n.kind AND i.party_ledger_id = n.party_ledger_id
             JOIN vouchers v ON v.id = i.voucher_id
             LEFT JOIN stock_note_order_sources src ON src.note_line_id = nl.id
             LEFT JOIN invoice_order_allocations dates ON dates.allocation_id = a.id
             WHERE nl.id = ?1 AND a.quantity = 0
               AND NOT EXISTS(SELECT 1 FROM invoice_order_sources o
                              WHERE o.allocation_id = a.id AND o.order_id IS NOT src.order_id)
               AND NOT EXISTS(SELECT 1 FROM invoice_reversals r WHERE r.invoice_id = i.id)
             ORDER BY dates.expected_on IS NULL, v.date, i.id, a.id",
        )?;
        let mut keys = self.references.into_iter().collect::<Vec<_>>();
        keys.sort_unstable();
        let mut matches = Vec::new();
        for reference in keys {
            let rows = notes
                .query_map(params![company, reference], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (note, note_line, note_date, mut available) in rows {
                if available <= 0 {
                    continue;
                }
                let bills = demands
                    .query_map([note_line], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut stock_line = None;
                for (invoice, invoice_allocation_id, invoice_date, remaining, due) in bills {
                    if available == 0 {
                        break;
                    }
                    if remaining <= 0
                        || (!self.notes.contains(&note_line)
                            && !self.invoices.contains(&invoice_allocation_id))
                    {
                        continue;
                    }
                    let mut quantity = remaining.min(available);
                    if let Some(due) = due {
                        if stock_line.is_none() {
                            stock_line = crate::stock_notes::note_lines(conn, company, note)?
                                .into_iter()
                                .find(|line| line.id == note_line);
                        }
                        let line = stock_line
                            .as_ref()
                            .ok_or_else(crate::stock_notes::invalid_spans)?;
                        quantity = quantity.min(crate::stock_notes::unbilled_on_delivery_date(
                            conn, line, &due,
                        )?);
                    }
                    if quantity == 0 {
                        continue;
                    }
                    crate::invoice_stock_links::link_imported_stock(
                        conn,
                        company,
                        note,
                        invoice,
                        note_date.as_str().max(invoice_date.as_str()),
                        &[StockNoteInvoiceAllocation {
                            invoice_allocation_id,
                            note_line_id: note_line,
                            quantity,
                        }],
                    )?;
                    matches.push(TrackingMatch {
                        note_line_id: note_line,
                        invoice_allocation_id,
                        quantity,
                    });
                    available -= quantity;
                    if let Some(line) = &mut stock_line {
                        line.billed += quantity;
                    }
                }
            }
        }
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actual_cost::ActualCostMethod;

    fn envelope(contents: &str) -> Vec<u8> {
        format!("<ENVELOPE><BODY><DATA><MESSAGE>{contents}</MESSAGE></DATA></BODY></ENVELOPE>")
            .into_bytes()
    }

    fn voucher(kind: &str, date: &str, reference: &str, details: &str) -> String {
        format!(
            "<VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME>
             <VOUCHERNUMBER>{reference}</VOUCHERNUMBER><PARTYLEDGERNAME>Party</PARTYLEDGERNAME>
             {details}</VOUCHER>"
        )
    }

    fn fixture(kind: InvoiceKind) -> (Store, i64, i64, i64) {
        let store = Store::memory().unwrap();
        let company = store.create_company("Tracking examples").unwrap();
        let (party, trade) = match kind {
            InvoiceKind::Sale => (Category::Asset, Category::Income),
            InvoiceKind::Purchase => (Category::Liability, Category::Expense),
        };
        store.create_ledger(company, "Party", party).unwrap();
        store.create_ledger(company, "Trade", trade).unwrap();
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        (store, company, item, location)
    }

    fn tracked_invoice(kind: &str, date: &str, sign: i64, mixed: bool) -> String {
        let batches = if mixed {
            format!("<BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>2 pcs</ACTUALQTY>
                       <BILLEDQTY>2 pcs</BILLEDQTY><AMOUNT>{}</AMOUNT></BATCHALLOCATIONS.LIST>
                     <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>1 pcs</ACTUALQTY>
                       <BILLEDQTY>1 pcs</BILLEDQTY><AMOUNT>{}</AMOUNT><TRACKINGNUMBER>Not Applicable</TRACKINGNUMBER>
                     </BATCHALLOCATIONS.LIST>", sign * 20, sign * 10)
        } else {
            String::new()
        };
        let amount = sign * 30;
        voucher(kind, date, "BILL", &format!(
            "<ISINVOICE>Yes</ISINVOICE>
             <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>{opposite}</AMOUNT>
               <BILLALLOCATIONS.LIST><NAME>BILL</NAME><BILLTYPE>New Ref</BILLTYPE><AMOUNT>{opposite}</AMOUNT></BILLALLOCATIONS.LIST>
             </LEDGERENTRIES.LIST>
             <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>3 pcs</ACTUALQTY>
               <BILLEDQTY>3 pcs</BILLEDQTY><RATE>10/pcs</RATE><AMOUNT>{amount}</AMOUNT>
               <TRACKINGNUMBER>Shipment-1</TRACKINGNUMBER>
               <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>{amount}</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
               {batches}
             </ALLINVENTORYENTRIES.LIST>",
            opposite = -amount
        ))
    }

    #[test]
    fn order_and_delivery_ledger_defaults_survive_until_the_invoice() {
        let (mut store, company, item, location) = fixture(InvoiceKind::Sale);
        let wholesale = store
            .create_ledger(company, "Wholesale", Category::Income)
            .unwrap();
        let retail = store
            .create_ledger(company, "Retail", Category::Income)
            .unwrap();
        let trade = find_ledger(&store.conn, company, "Trade").unwrap().unwrap();
        let party = find_ledger(&store.conn, company, "Party").unwrap().unwrap();
        store
            .adjust_stock_at(company, item, location, "2026-04-01", 5000, "Opening")
            .unwrap();
        let order = voucher(
            "Sales Order",
            "20260401",
            "ORDER",
            r#"
            <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>
              <ACTUALQTY>2 pcs</ACTUALQTY><RATE>10/pcs</RATE><AMOUNT>20</AMOUNT>
              <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Wholesale</LEDGERNAME><AMOUNT>20</AMOUNT>
              </ACCOUNTINGALLOCATIONS.LIST>
            </INVENTORYENTRIES.LIST>"#,
        );
        let inherited = voucher(
            "Delivery Note",
            "20260402",
            "INHERITED",
            r#"
            <ORDERNO>ORDER</ORDERNO>
            <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>
              <ACTUALQTY>-1 pcs</ACTUALQTY><AMOUNT>10</AMOUNT>
            </INVENTORYENTRIES.LIST>"#,
        );
        let override_note = voucher(
            "Delivery Note",
            "20260403",
            "OVERRIDE",
            r#"
            <ALLLEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><ISPARTYLEDGER>Yes</ISPARTYLEDGER>
              <AMOUNT>-30</AMOUNT></ALLLEDGERENTRIES.LIST>
            <ALLLEDGERENTRIES.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>20</AMOUNT>
              </ALLLEDGERENTRIES.LIST>
            <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>
              <ACTUALQTY>-3 pcs</ACTUALQTY><AMOUNT>30</AMOUNT>
              <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>-2 pcs</ACTUALQTY>
                <AMOUNT>20</AMOUNT>
              </BATCHALLOCATIONS.LIST>
              <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>-1 pcs</ACTUALQTY>
                <AMOUNT>10</AMOUNT><ORDERNO>ORDER</ORDERNO>
                <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Retail</LEDGERNAME>
                  <AMOUNT>10</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
              </BATCHALLOCATIONS.LIST>
            </INVENTORYENTRIES.LIST>"#,
        );
        let plan = store
            .preview_xml_import(company, &envelope(&(order + &inherited + &override_note)))
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        assert!(store.vouchers(company).unwrap().is_empty());
        let order = store.orders(company).unwrap().remove(0);
        assert_eq!(
            store.order_detail(company, order.id).unwrap().lines[0].trade_ledger_id,
            Some(wholesale)
        );

        let notes = store.stock_notes(company, None).unwrap();
        let read_lines = |reference: &str| {
            let note = notes
                .iter()
                .find(|note| note.reference == reference)
                .unwrap();
            store.stock_note_lines(company, note.id).unwrap()
        };
        let inherited = read_lines("INHERITED").remove(0);
        let overridden = read_lines("OVERRIDE");
        assert_eq!(inherited.trade_ledger.as_deref(), Some("Wholesale"));
        assert_eq!(
            overridden
                .iter()
                .map(|line| line.trade_ledger_id)
                .collect::<Vec<_>>(),
            [Some(trade), Some(retail)]
        );
        assert_eq!(overridden[1].order_id, Some(order.id));

        let sources = [inherited, overridden[1].clone()];
        let invoice = InvoiceDraft {
            kind: InvoiceKind::Sale,
            entry_mode: InvoiceEntryMode::Invoice,
            date: "2026-04-04".into(),
            reference: "STOCK-BILL".into(),
            external_reference: Default::default(),
            due_date: None,
            credit_days: None,
            bill_reference: None,
            bill_splits: Vec::new(),
            narration: String::new(),
            party_ledger_id: party,
            trade_ledger_id: trade,
            tax_ledger_id: None,
            tax_amount: 0,
            additional_taxes: Vec::new(),
            tax_calculation: None,
            lines: sources
                .iter()
                .map(|line| InvoiceLine {
                    item_id: item,
                    trade_ledger_id: line.trade_ledger_id,
                    location_id: location,
                    batch_id: None,
                    extra_allocations: Vec::new(),
                    discount: InvoiceDiscount::None,
                    quantity: line.values.quantity,
                    billed_quantity: None,
                    unit_price: line.values.unit_price,
                    rate_basis: None,
                })
                .collect(),
            adjustments: Vec::new(),
        };
        let ids = sources.map(|line| line.id);
        store.invoice_stock_notes(company, &ids, &invoice).unwrap();
        let posted = store.invoices(company).unwrap().remove(0);
        assert_eq!(posted.total, 2000);
        assert_eq!(
            posted
                .lines
                .iter()
                .map(|line| line.trade_ledger.as_str())
                .collect::<Vec<_>>(),
            ["Wholesale", "Retail"]
        );
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            1000
        );
    }

    #[test]
    fn receipt_ledger_annotations_leave_stock_costs_and_accounts_independent() {
        let (mut store, company, _, _) = fixture(InvoiceKind::Purchase);
        let capital = store
            .create_ledger(company, "Equipment", Category::Asset)
            .unwrap();
        let receipt = voucher(
            "Receipt Note",
            "20260401",
            "RECEIPT",
            r#"
            <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>30</AMOUNT></LEDGERENTRIES.LIST>
            <LEDGERENTRIES.LIST><LEDGERNAME>Equipment</LEDGERNAME><AMOUNT>-30</AMOUNT></LEDGERENTRIES.LIST>
            <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>3 pcs</ACTUALQTY>
              <AMOUNT>-24</AMOUNT><RATE>8/pcs</RATE>
            </INVENTORYENTRIES.LIST>"#,
        );
        let unsupported = receipt.replace(
            "<LEDGERNAME>Equipment</LEDGERNAME>",
            "<LEDGERNAME>Unavailable</LEDGERNAME>",
        );
        let plan = store
            .preview_xml_import(company, &envelope(&unsupported))
            .unwrap();
        assert!(!plan.can_apply());
        assert!(store.stock_notes(company, None).unwrap().is_empty());
        let plan = store
            .preview_xml_import(company, &envelope(&receipt))
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let note = store.stock_notes(company, None).unwrap().remove(0);
        let line = store.stock_note_lines(company, note.id).unwrap().remove(0);
        assert_eq!(line.trade_ledger_id, Some(capital));
        assert_eq!(line.values.unit_price, 800);
        assert!(store.vouchers(company).unwrap().is_empty());
        let stock = store.stock_receipts(company, None).unwrap();
        assert_eq!(stock.len(), 1);
        assert_eq!((stock[0].quantity, stock[0].cost), (3000, Some(2400)));
    }

    #[test]
    fn tracked_deliveries_can_follow_an_invoice_in_separate_imports() {
        let (mut store, company, item, location) = fixture(InvoiceKind::Sale);
        let receipt = store
            .adjust_stock_at(company, item, location, "2026-04-01", 3000, "Opening")
            .unwrap();
        store
            .set_stock_receipt_cost(company, receipt, 1500, "Opening cost")
            .unwrap();
        let invoice_xml = envelope(&tracked_invoice("Sales", "20260402", 1, false));
        let plan = store.preview_xml_import(company, &invoice_xml).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert!(store.invoices(company).unwrap().is_empty());
        store.apply_xml_import(&plan).unwrap();
        let invoice = store.invoices(company).unwrap().remove(0);
        assert_eq!((invoice.total, invoice.lines[0].quantity), (3000, 0));
        assert_eq!(
            invoice.lines[0].allocations[0].source_tracking.as_deref(),
            Some("Shipment-1")
        );
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            3000
        );
        let stored: i64 = store
            .conn
            .query_row(
                "SELECT reported_stock_quantity FROM imported_invoice_tracking",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, 3000);
        let draft = store.invoice_draft(company, invoice.id).unwrap();
        assert!(store.update_invoice(company, invoice.id, &draft).is_err());
        for (index, date, quantity) in [(1, "20260403", 1), (2, "20260404", 2)] {
            let note_xml = envelope(&voucher("Delivery Note", date, &format!("DELIVERY-{index}"), &format!(
                "<ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>-{quantity} pcs</ACTUALQTY>
                 <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>-{quantity} pcs</ACTUALQTY>
                   <TRACKINGNUMBER>shipment-1</TRACKINGNUMBER></BATCHALLOCATIONS.LIST></ALLINVENTORYENTRIES.LIST>"
            )));
            let plan = store.preview_xml_import(company, &note_xml).unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            assert_eq!(plan.stock_note_count(), 1);
            assert_eq!(store.apply_xml_import(&plan).unwrap().stock_notes, 1);
            assert!(!store
                .preview_xml_import(company, &note_xml)
                .unwrap()
                .can_apply());
        }
        let invoice = store.invoices(company).unwrap().remove(0);
        assert_eq!(
            (
                invoice.outstanding,
                invoice.lines[0].allocations[0].linked_stock_quantity
            ),
            (3000, 3000)
        );
        assert_eq!(invoice.stock_note_ids.len(), 2);
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            0
        );
        assert_eq!(
            store
                .sales_at_actual_cost(company, None, None, ActualCostMethod::Fifo)
                .unwrap()
                .cost_of_sales,
            1500
        );
    }

    #[test]
    fn receipt_before_invoice_reconciles_mixed_allocations_without_posting_twice() {
        let (mut store, company, item, _) = fixture(InvoiceKind::Purchase);
        let receipt = voucher("Receipt Note", "20260401", "RECEIPT", "
            <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
              <AMOUNT>-16.03</AMOUNT><BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><BATCHNAME>Actual lot</BATCHNAME>
                <ACTUALQTY>2 pcs</ACTUALQTY><AMOUNT>-16.03</AMOUNT><TRACKINGNUMBER>Shipment-1</TRACKINGNUMBER>
              </BATCHALLOCATIONS.LIST></INVENTORYENTRIES.LIST>");
        let purchase = tracked_invoice("Purchase", "20260402", -1, true);
        let mixed = envelope(&(purchase + &receipt));
        let bad = String::from_utf8(mixed.clone()).unwrap().replace(
            "<BATCHNAME>Actual lot</BATCHNAME>\n                <ACTUALQTY>2 pcs",
            "<BATCHNAME>Actual lot</BATCHNAME>\n                <ACTUALQTY>1 pcs",
        );
        let invalid = store.preview_xml_import(company, bad.as_bytes()).unwrap();
        assert!(invalid
            .issues()
            .iter()
            .any(|issue| issue.contains("batch quantities do not match")));
        assert!(store.stock_notes(company, None).unwrap().is_empty());
        let plan = store.preview_xml_import(company, &mixed).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            0
        );
        store.apply_xml_import(&plan).unwrap();
        let invoice = store.invoices(company).unwrap().remove(0);
        let line = &invoice.lines[0];
        assert_eq!(
            (invoice.total, line.quantity, line.billed_quantity),
            (3000, 1000, 3000)
        );
        assert_eq!(
            (
                line.allocations[0].linked_stock_quantity,
                line.allocations[1].linked_stock_quantity
            ),
            (2000, 0)
        );
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            3000
        );
        assert_eq!(
            store
                .stock_valuation_at_average_cost(company, "2026-04-02")
                .unwrap()
                .known_value,
            2603
        );
        let report = store
            .receipt_reconciliation(company, None)
            .unwrap()
            .remove(0);
        assert_eq!(
            (
                report.invoiced_quantity,
                report.invoice_net,
                report.invoiced_receipt_cost
            ),
            (2000, 2000, Some(1603))
        );
    }

    #[test]
    fn stale_preview_leaves_stock_and_note_history_untouched() {
        let (mut store, company, item, _) = fixture(InvoiceKind::Purchase);
        let purchase = envelope(&tracked_invoice("Purchase", "20260401", -1, false));
        let plan = store.preview_xml_import(company, &purchase).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let bill = store.invoices(company).unwrap()[0].id;
        let stock = envelope(&voucher(
            "Receipt Note",
            "20260402",
            "RECEIPT",
            "
            <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>3 pcs</ACTUALQTY>
            <TRACKINGNUMBER>Shipment-1</TRACKINGNUMBER><RATE>10.00/pcs</RATE>
            </INVENTORYENTRIES.LIST>",
        ));
        let plan = store.preview_xml_import(company, &stock).unwrap();
        assert_eq!(plan.stock_match_count(), 1);
        store
            .reverse_invoice(company, bill, "2026-04-03", "VOID", "Cancel bill")
            .unwrap();
        assert!(store
            .apply_xml_import(&plan)
            .unwrap_err()
            .to_string()
            .contains("changed since preview"));
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            0
        );
        assert!(store.stock_notes(company, None).unwrap().is_empty());
        let new = store.preview_xml_import(company, &stock).unwrap();
        assert!(new.can_apply(), "{:?}", new.issues());
        assert_eq!(new.stock_match_count(), 0);
        store.apply_xml_import(&new).unwrap();
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            3000
        );
        assert_eq!(
            store.stock_notes(company, None).unwrap()[0].status(),
            "Awaiting invoice"
        );
    }
}
