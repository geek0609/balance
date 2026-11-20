//! Warehouse movements from inventory and manufacturing journal vouchers.
use super::*;
use crate::stock_journals::{AllocationInput, EntryInput, JournalInput};

#[derive(Debug, Clone)]
pub(super) struct ImportedJournal {
    pub date: String,
    pub source_order: usize,
    voucher_type: String,
    reference: String,
    narration: String,
    entries: Vec<JournalEntry>,
    metadata: SourceMetadata,
}

#[derive(Debug, Clone)]
struct JournalEntry {
    item: String,
    existing_id: Option<i64>,
    unit: String,
    precision: u8,
    quantity: i64,
    value: RecordedValue,
    allocations: Vec<(ImportedStockAllocation, RecordedValue)>,
}

#[derive(Debug, Clone, Default)]
struct RecordedValue {
    rate: Option<String>,
    amount: Option<i64>,
}

pub(super) fn kind(value: &str) -> bool {
    matches!(
        normalized(value).as_str(),
        "stock journal" | "manufacturing journal"
    )
}

pub(super) fn type_name(node: Node<'_, '_>) -> Option<String> {
    let name = object_name(node)?;
    let parent = direct_text(node, "PARENT");
    let journal_alias = parent.as_deref().is_some_and(kind)
        && parse_kind(&name).is_none()
        && stock_notes::kind(&name).is_none()
        && orders::kind(&name).is_none()
        && stock_rejections::kind(&name).is_none()
        && !stock_counts::kind(&name);
    (kind(&name) || journal_alias).then_some(name)
}

pub(super) fn parse(
    node: Node<'_, '_>,
    source_order: usize,
    voucher_type: String,
    label: &str,
    metadata: SourceMetadata,
    items: &InvoiceStockItems,
    locations: &HashMap<String, Option<i64>>,
    batches: &HashMap<(String, String), i64>,
    issues: &mut Vec<String>,
) -> Result<ImportedJournal> {
    if node.children().any(|entry| {
        entry.is_element()
            && meaningful(entry)
            && matches!(
                local_name(entry).to_ascii_uppercase().as_str(),
                "LEDGERENTRIES.LIST" | "ALLLEDGERENTRIES.LIST"
            )
    }) {
        return Err(Error::Validation(
            "Stock journals with additional costs or ledger entries require valuation review"
                .into(),
        ));
    }
    check_fields(
        node,
        &[
            "DATE",
            "VOUCHERTYPENAME",
            "VOUCHERNUMBER",
            "NARRATION",
            "PERSISTEDVIEW",
            "OBJVIEW",
            "ISINVOICE",
            "ISCANCELLED",
            "ISOPTIONAL",
            "INVENTORYENTRIESIN.LIST",
            "INVENTORYENTRIESOUT.LIST",
        ],
        label,
        issues,
    );
    check_voucher_view(
        node,
        &["Inventory Voucher View", "Manufacturing Journal View"],
    )?;
    for flag in ["ISCANCELLED", "ISOPTIONAL"] {
        if direct_text(node, flag)
            .is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("no"))
        {
            return Err(Error::Validation(
                "Cancelled and optional stock journals cannot be posted".into(),
            ));
        }
    }
    let date = xml_date(&invoice_field(node, "DATE")?)?;
    let reference = invoice_field(node, "VOUCHERNUMBER")?;
    let mut entries = Vec::new();
    for entry in node
        .children()
        .filter(|entry| entry.is_element() && meaningful(*entry))
    {
        let direction = match local_name(entry).to_ascii_uppercase().as_str() {
            "INVENTORYENTRIESIN.LIST" => 1,
            "INVENTORYENTRIESOUT.LIST" => -1,
            _ => continue,
        };
        invoice_list_fields(
            entry,
            &[
                "STOCKITEMNAME",
                "ACTUALQTY",
                "BILLEDQTY",
                "GODOWNNAME",
                "BATCHNAME",
                "ISDEEMEDPOSITIVE",
                "ISLASTDEEMEDPOSITIVE",
                "RATE",
                "AMOUNT",
                "BATCHALLOCATIONS.LIST",
            ],
            label,
            issues,
        );
        let item = invoice_field(entry, "STOCKITEMNAME")?;
        let item_key = normalized(&item);
        let (existing_id, unit) = items
            .get(&item_key)
            .ok_or_else(|| Error::Validation(format!("Stock item {item} is unavailable")))?;
        let quantity = journal_quantity(entry, unit)?;
        let mut parts = entry
            .children()
            .filter(|part| {
                part.is_element()
                    && meaningful(*part)
                    && local_name(*part).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
            })
            .collect::<Vec<_>>();
        if parts.is_empty() {
            parts.push(entry);
        }
        let mut total = 0_i128;
        let mut allocations = Vec::with_capacity(parts.len());
        for part in parts {
            let allocated = if part == entry {
                quantity
            } else {
                invoice_list_fields(
                    part,
                    &[
                        "ACTUALQTY",
                        "BILLEDQTY",
                        "GODOWNNAME",
                        "BATCHNAME",
                        "ISDEEMEDPOSITIVE",
                        "ISLASTDEEMEDPOSITIVE",
                        "RATE",
                        "AMOUNT",
                    ],
                    label,
                    issues,
                );
                journal_quantity(part, unit)?
            };
            total += i128::from(allocated);
            let named = |field| {
                direct_text(part, field)
                    .filter(|value| !value.is_empty())
                    .or_else(|| direct_text(entry, field).filter(|value| !value.is_empty()))
            };
            let requested = named("GODOWNNAME").unwrap_or_default();
            let (location, existing_location_id) = xml_stock_location(&requested, locations)
                .ok_or_else(|| {
                    Error::Validation(format!("Stock location {requested} is unavailable"))
                })?;
            let batch = stock_notes::tracking(named("BATCHNAME"));
            let existing_batch_id = batch
                .as_deref()
                .and_then(|name| batches.get(&(item_key.clone(), normalized(name))).copied());
            allocations.push((
                ImportedStockAllocation {
                    location,
                    existing_location_id,
                    batch,
                    existing_batch_id,
                    quantity: allocated * direction,
                    billed_quantity: 0,
                    tracking: None,
                },
                if part == entry {
                    RecordedValue::default()
                } else {
                    value(part)?
                },
            ));
        }
        if total != i128::from(quantity) {
            return Err(Error::Validation(
                "Stock journal allocations do not add up to the item quantity".into(),
            ));
        }
        let mut entry_value = value(entry)?;
        if entry_value.amount.is_none() {
            let allocation_total = allocations.iter().try_fold(0_i128, |total, (_, recorded)| {
                recorded.amount.map(|amount| total + i128::from(amount))
            });
            entry_value.amount = allocation_total
                .map(i64::try_from)
                .transpose()
                .map_err(|_| Error::Validation("Stock journal amount is too large".into()))?;
        }
        entries.push(JournalEntry {
            item,
            existing_id: *existing_id,
            unit: unit.symbol.clone(),
            precision: unit.precision,
            quantity: quantity * direction,
            value: entry_value,
            allocations,
        });
    }
    if entries.is_empty() {
        return Err(Error::Validation(
            "Stock journal has no inventory lines".into(),
        ));
    }
    Ok(ImportedJournal {
        date,
        source_order,
        voucher_type,
        reference,
        narration: direct_text(node, "NARRATION").unwrap_or_default(),
        entries,
        metadata,
    })
}

fn journal_quantity(node: Node<'_, '_>, unit: &UnitMeasure) -> Result<i64> {
    let magnitude = |text: &str| {
        unit.quantity(text)?
            .checked_abs()
            .ok_or_else(|| Error::Validation("Stock journal quantity is too large".into()))
    };
    let quantity = magnitude(&invoice_field(node, "ACTUALQTY")?)?;
    if quantity == 0 || quantity % 10_i64.pow(u32::from(3 - unit.precision)) != 0 {
        return Err(Error::Validation(
            "Stock journal quantity must be nonzero and match the stock unit precision".into(),
        ));
    }
    if let Some(value) = direct_text(node, "BILLEDQTY").filter(|value| !value.is_empty()) {
        let billed = magnitude(&value)?;
        if billed != 0 && billed != quantity {
            return Err(Error::Validation(
                "Stock journal quantities disagree".into(),
            ));
        }
    }
    Ok(quantity)
}

fn value(node: Node<'_, '_>) -> Result<RecordedValue> {
    let amount = direct_text(node, "AMOUNT")
        .filter(|value| !value.is_empty())
        .map(|value| {
            xml_amount(&value)?
                .checked_abs()
                .ok_or_else(|| Error::Validation("Stock journal amount is too large".into()))
        })
        .transpose()?;
    Ok(RecordedValue {
        rate: direct_text(node, "RATE").filter(|rate| !rate.is_empty()),
        amount,
    })
}

pub(super) fn write(
    conn: &Connection,
    company: i64,
    source: &ImportedJournal,
    created_batches: &mut HashMap<(i64, String), i64>,
) -> Result<()> {
    let mut entries = Vec::with_capacity(source.entries.len());
    for entry in &source.entries {
        let item = resolve_imported_item(
            conn,
            company,
            &entry.item,
            entry.existing_id,
            &entry.unit,
            entry.precision,
        )?;
        let mut allocations = Vec::with_capacity(entry.allocations.len());
        for (allocation, value) in &entry.allocations {
            let (location, batch) =
                resolve_imported_allocation(conn, company, item, allocation, created_batches)?;
            allocations.push(AllocationInput {
                location,
                batch,
                quantity: allocation.quantity,
                rate: value.rate.as_deref(),
                amount: value.amount,
            });
        }
        entries.push(EntryInput {
            item,
            quantity: entry.quantity,
            rate: entry.value.rate.as_deref(),
            amount: entry.value.amount,
            allocations,
        });
    }
    let saved = crate::stock_journals::write(
        conn,
        company,
        JournalInput {
            date: &source.date,
            voucher_type: &source.voucher_type,
            reference: &source.reference,
            narration: &source.narration,
            entries,
        },
    )?;
    save_metadata(conn, company, "stockjournal", saved, &source.metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actual_cost::{ActualCostIssue, ActualCostMethod};
    use crate::stock_counts::StockCountMode;

    fn envelope(content: &str) -> String {
        format!("<ENVELOPE><BODY><DATA><MESSAGE>{content}</MESSAGE></DATA></BODY></ENVELOPE>")
    }

    fn opening(
        store: &mut Store,
        company: i64,
        item: i64,
        location: i64,
        batch: Option<i64>,
        quantity: i64,
        value: i64,
    ) {
        let receipt = store
            .adjust_stock_batch(
                company,
                item,
                location,
                batch,
                "2026-04-01",
                quantity,
                "Opening",
            )
            .unwrap();
        store
            .set_stock_receipt_cost(company, receipt, value, "Opening value")
            .unwrap();
    }

    #[test]
    fn preserves_warehouse_and_batch_legs_before_a_same_day_count() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Warehouse trial").unwrap();
        let part = store.create_stock_item(company, "Part", "pcs").unwrap();
        let main = store.stock_locations(company).unwrap()[0].id;
        let west = store.create_stock_location(company, "West").unwrap();
        let north = store.create_stock_location(company, "North").unwrap();
        let lot = store
            .create_stock_batch(company, part, "Lot A", None, None)
            .unwrap();
        opening(&mut store, company, part, main, Some(lot), 8000, 8000);
        let xml = envelope(
            r#"<VOUCHERTYPE NAME="Warehouse Move"><PARENT>Stock Journal</PARENT></VOUCHERTYPE>
            <VOUCHER VCHTYPE="Warehouse Move" OBJVIEW="Inventory Voucher View">
              <DATE>20260402</DATE><VOUCHERNUMBER>MOVE-1</VOUCHERNUMBER><NARRATION>Split shipment</NARRATION>
              <GUID>warehouse-move-1</GUID>
              <INVENTORYENTRIESOUT.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>
                <ACTUALQTY>-5 pcs</ACTUALQTY><RATE>10/pcs</RATE><AMOUNT>-50</AMOUNT>
                <BATCHALLOCATIONS.LIST><GODOWNNAME>Main Location</GODOWNNAME><BATCHNAME>Lot A</BATCHNAME>
                  <ACTUALQTY>-5 pcs</ACTUALQTY>
                </BATCHALLOCATIONS.LIST>
              </INVENTORYENTRIESOUT.LIST>
              <INVENTORYENTRIESIN.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>
                <ACTUALQTY>5 pcs</ACTUALQTY><AMOUNT>50</AMOUNT>
                <BATCHALLOCATIONS.LIST><GODOWNNAME>West</GODOWNNAME><BATCHNAME>Lot B</BATCHNAME>
                  <ACTUALQTY>2 pcs</ACTUALQTY><AMOUNT>20</AMOUNT>
                </BATCHALLOCATIONS.LIST>
                <BATCHALLOCATIONS.LIST><GODOWNNAME>North</GODOWNNAME><BATCHNAME>Lot A</BATCHNAME>
                  <ACTUALQTY>3 pcs</ACTUALQTY><AMOUNT>30</AMOUNT>
                </BATCHALLOCATIONS.LIST>
              </INVENTORYENTRIESIN.LIST>
            </VOUCHER>
            <VOUCHER><VOUCHERTYPENAME>Physical Stock</VOUCHERTYPENAME>
              <DATE>20260402</DATE><VOUCHERNUMBER>COUNT-1</VOUCHERNUMBER>
              <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>
                <ACTUALQTY>2 pcs</ACTUALQTY><GODOWNNAME>West</GODOWNNAME><BATCHNAME>Lot B</BATCHNAME>
              </INVENTORYENTRIES.LIST>
            </VOUCHER>"#,
        );
        let plan = store
            .preview_xml_import_batch_with_stock_counts(
                company,
                &[xml.as_bytes()],
                None,
                Some(StockCountMode::Reconcile),
            )
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert_eq!(plan.stock_journal_count(), 1);
        let imported = store.apply_xml_import(&plan).unwrap();
        assert_eq!((imported.stock_journals, imported.stock_counts), (1, 1));
        let journal = store.stock_journals(company).unwrap().remove(0);
        assert_eq!(journal.voucher_type, "Warehouse Move");
        assert_eq!(journal.narration, "Split shipment");
        let entries = store.stock_journal_entries(company, journal.id).unwrap();
        assert_eq!(entries[0].quantity, -5000);
        assert_eq!(entries[0].rate.as_deref(), Some("10/pcs"));
        assert_eq!(entries[1].allocations[0].batch.as_deref(), Some("Lot B"));
        assert_eq!(entries[1].allocations[0].amount, Some(2000));
        for (location, quantity) in [(main, 3000), (west, 2000), (north, 3000)] {
            let register = store
                .stock_register_at(company, part, Some(location), None, None)
                .unwrap();
            assert_eq!(register.closing_quantity, quantity);
            assert!(register
                .movements
                .iter()
                .any(|row| row.description.contains("MOVE-1")));
        }
        let count = store.stock_counts(company).unwrap().remove(0);
        let counted = store
            .stock_count_lines(company, count.id)
            .unwrap()
            .remove(0);
        assert_eq!((counted.book_quantity, counted.adjustment_id), (2000, None));
        for method in [ActualCostMethod::Fifo, ActualCostMethod::WeightedAverage] {
            let report = store
                .stock_valuation_at_actual_cost(company, "2026-04-02", method)
                .unwrap();
            assert_eq!(report.known_value, 8000);
            assert_eq!(report.unavailable, 0);
        }
        assert!(store.vouchers(company).unwrap().is_empty());
        assert!(store.apply_xml_import(&plan).is_err());
        assert_eq!(store.stock_journals(company).unwrap().len(), 1);
        let spillage = envelope(
            r#"<VOUCHER><DATE>20260403</DATE><VOUCHERTYPENAME>Stock Journal</VOUCHERTYPENAME>
              <VOUCHERNUMBER>LOSS-1</VOUCHERNUMBER>
              <INVENTORYENTRIESOUT.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
                <GODOWNNAME>Main</GODOWNNAME><BATCHNAME>Lot A</BATCHNAME>
              </INVENTORYENTRIESOUT.LIST>
              <INVENTORYENTRIESIN.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>1 pcs</ACTUALQTY>
                <GODOWNNAME>West</GODOWNNAME><BATCHNAME>Lot B</BATCHNAME>
              </INVENTORYENTRIESIN.LIST>
            </VOUCHER>"#,
        );
        let plan = store
            .preview_xml_import(company, spillage.as_bytes())
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let report = store
            .stock_valuation_at_actual_cost(company, "2026-04-03", ActualCostMethod::Fifo)
            .unwrap();
        assert_eq!(report.rows[0].quantity, 7000);
        assert_eq!(report.known_value, 7000);
    }

    #[test]
    fn preserves_multiple_production_outputs_and_reports_missing_value() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Production trial").unwrap();
        let main = store.stock_locations(company).unwrap()[0].id;
        let raw = store.create_stock_item(company, "Raw", "pcs").unwrap();
        store.create_stock_item(company, "Finished", "pcs").unwrap();
        store.create_stock_item(company, "Scrap", "pcs").unwrap();
        opening(&mut store, company, raw, main, None, 4000, 3200);
        let xml = envelope(
            r#"<VOUCHER><DATE>20260402</DATE><VOUCHERTYPENAME>Manufacturing Journal</VOUCHERTYPENAME>
              <VOUCHERNUMBER>RUN-1</VOUCHERNUMBER>
              <INVENTORYENTRIESOUT.LIST><STOCKITEMNAME>Raw</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
              </INVENTORYENTRIESOUT.LIST>
              <INVENTORYENTRIESIN.LIST><STOCKITEMNAME>Finished</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
                <BATCHALLOCATIONS.LIST><BATCHNAME>Lot 1</BATCHNAME><ACTUALQTY>1 pcs</ACTUALQTY>
                  <AMOUNT>18.25</AMOUNT>
                </BATCHALLOCATIONS.LIST>
                <BATCHALLOCATIONS.LIST><BATCHNAME>Lot 2</BATCHNAME><ACTUALQTY>1 pcs</ACTUALQTY>
                  <AMOUNT>5.75</AMOUNT>
                </BATCHALLOCATIONS.LIST>
              </INVENTORYENTRIESIN.LIST>
              <INVENTORYENTRIESIN.LIST><STOCKITEMNAME>Scrap</STOCKITEMNAME><ACTUALQTY>3 pcs</ACTUALQTY>
              </INVENTORYENTRIESIN.LIST>
            </VOUCHER>"#,
        );
        let plan = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let journal = store.stock_journals(company).unwrap().remove(0);
        let rows = store
            .stock_valuation_at_actual_cost(company, "2026-04-02", ActualCostMethod::Fifo)
            .unwrap()
            .rows;
        let item = |name| rows.iter().find(|row| row.item.name == name).unwrap();
        assert_eq!(
            (item("Raw").quantity, item("Raw").value),
            (2000, Some(1600))
        );
        assert_eq!(
            (item("Finished").quantity, item("Finished").value),
            (2000, Some(2400))
        );
        assert_eq!(item("Scrap").quantity, 3000);
        assert!(
            matches!(item("Scrap").issue, Some(ActualCostIssue::MissingJournalCost { journal_id, .. }) if journal_id == journal.id)
        );
    }

    #[test]
    fn validates_allocation_totals_and_rechecks_current_stock_policy_on_apply() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Validation trial").unwrap();
        let part = store.create_stock_item(company, "Part", "pcs").unwrap();
        let contents = r#"<VOUCHER><DATE>20260402</DATE><VOUCHERTYPENAME>Stock Journal</VOUCHERTYPENAME>
              <VOUCHERNUMBER>LOSS-1</VOUCHERNUMBER>
              <INVENTORYENTRIESOUT.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
                <BATCHALLOCATIONS.LIST><ACTUALQTY>2 pcs</ACTUALQTY></BATCHALLOCATIONS.LIST>
              </INVENTORYENTRIESOUT.LIST>
            </VOUCHER>"#;
        let invalid = envelope(&contents.replace(
            "<BATCHALLOCATIONS.LIST><ACTUALQTY>2 pcs",
            "<BATCHALLOCATIONS.LIST><ACTUALQTY>1 pcs",
        ));
        let plan = store
            .preview_xml_import(company, invalid.as_bytes())
            .unwrap();
        assert!(plan
            .issues()
            .iter()
            .any(|issue| issue.contains("do not add up")));

        let charged = envelope(&contents.replace(
            "</VOUCHER>",
            "<LEDGERENTRIES.LIST><LEDGERNAME>Freight</LEDGERNAME><AMOUNT>10</AMOUNT></LEDGERENTRIES.LIST></VOUCHER>",
        ));
        let preview = store
            .preview_xml_import(company, charged.as_bytes())
            .unwrap();
        assert!(!preview.can_apply());
        assert!(preview
            .issues()
            .iter()
            .any(|issue| issue.contains("require valuation review")));

        let xml = envelope(contents);
        let plan = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store
            .set_negative_stock_policy(company, Some("2026-04-01"), "Policy changed")
            .unwrap();
        let error = store.apply_xml_import(&plan).unwrap_err().to_string();
        assert!(error.contains("Negative stock is blocked"), "{error}");
        assert!(store.stock_journals(company).unwrap().is_empty());
        assert_eq!(
            store
                .stock_register(company, part, None, None)
                .unwrap()
                .closing_quantity,
            0
        );
    }
}
