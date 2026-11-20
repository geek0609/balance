//! Physical quantity counts from dated inventory vouchers.
use super::*;
use crate::stock_counts::{
    prepare_stock_count, write_stock_count, CountInput, CountLine, StockCountMode,
};

#[derive(Debug, Clone)]
pub(super) struct ImportedCount {
    pub date: String,
    pub source_order: usize,
    reference: String,
    notes: String,
    mode: StockCountMode,
    lines: Vec<CountedStock>,
    metadata: SourceMetadata,
}

#[derive(Debug, Clone)]
struct CountedStock {
    item: String,
    existing_id: Option<i64>,
    unit: String,
    precision: u8,
    allocation: ImportedStockAllocation,
}

pub(super) fn kind(value: &str) -> bool {
    normalized(value) == "physical stock"
}

pub(super) fn parse(
    node: Node<'_, '_>,
    source_order: usize,
    mode: StockCountMode,
    label: &str,
    metadata: SourceMetadata,
    stock: &InvoiceStockItems,
    locations: &HashMap<String, Option<i64>>,
    batches: &HashMap<(String, String), i64>,
    issues: &mut Vec<String>,
) -> Result<ImportedCount> {
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
            "ALLINVENTORYENTRIES.LIST",
            "INVENTORYENTRIES.LIST",
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
                "Cancelled and optional stock counts cannot be posted".into(),
            ));
        }
    }
    let date = xml_date(&invoice_field(node, "DATE")?)?;
    let reference = invoice_field(node, "VOUCHERNUMBER")?;
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for entry in node.children().filter(|entry| {
        entry.is_element()
            && meaningful(*entry)
            && matches!(
                local_name(*entry).to_ascii_uppercase().as_str(),
                "ALLINVENTORYENTRIES.LIST" | "INVENTORYENTRIES.LIST"
            )
    }) {
        invoice_list_fields(
            entry,
            &[
                "STOCKITEMNAME",
                "ACTUALQTY",
                "BILLEDQTY",
                "GODOWNNAME",
                "BATCHNAME",
                "ISDEEMEDPOSITIVE",
                "RATE",
                "AMOUNT",
                "BATCHALLOCATIONS.LIST",
            ],
            label,
            issues,
        );
        let item = invoice_field(entry, "STOCKITEMNAME")?;
        let item_key = normalized(&item);
        let (existing_id, unit) = stock
            .get(&item_key)
            .ok_or_else(|| Error::Validation(format!("Stock item {item} is unavailable")))?;
        let quantity = counted_quantity(entry, unit)?;
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
        for part in parts {
            let value = if part == entry {
                quantity
            } else {
                invoice_list_fields(
                    part,
                    &[
                        "ACTUALQTY",
                        "BILLEDQTY",
                        "GODOWNNAME",
                        "BATCHNAME",
                        "RATE",
                        "AMOUNT",
                        "ISDEEMEDPOSITIVE",
                    ],
                    label,
                    issues,
                );
                counted_quantity(part, unit)?
            };
            total += i128::from(value);
            let named = |field| {
                direct_text(part, field)
                    .filter(|value| !value.is_empty())
                    .or_else(|| direct_text(entry, field).filter(|value| !value.is_empty()))
            };
            let location = named("GODOWNNAME").unwrap_or_else(|| "Main".into());
            let location_key = normalized(&location);
            let existing_location_id = *locations.get(&location_key).ok_or_else(|| {
                Error::Validation(format!("Stock location {location} is unavailable"))
            })?;
            let batch = stock_notes::tracking(named("BATCHNAME"));
            let batch_key = batch.as_deref().map(normalized);
            if !seen.insert((item_key.clone(), location_key, batch_key.clone())) {
                return Err(Error::Validation(
                    "A stock count lists the same item, location and batch more than once".into(),
                ));
            }
            let existing_batch_id =
                batch_key.and_then(|key| batches.get(&(item_key.clone(), key)).copied());
            lines.push(CountedStock {
                item: item.clone(),
                existing_id: *existing_id,
                unit: unit.symbol.clone(),
                precision: unit.precision,
                allocation: ImportedStockAllocation {
                    location,
                    existing_location_id,
                    batch,
                    existing_batch_id,
                    quantity: value,
                    billed_quantity: 0,
                    tracking: None,
                },
            });
        }
        if total != i128::from(quantity) {
            return Err(Error::Validation(
                "Stock count allocations do not add up to the item quantity".into(),
            ));
        }
    }
    if lines.is_empty() {
        return Err(Error::Validation(
            "Stock count has no inventory lines".into(),
        ));
    }
    Ok(ImportedCount {
        date,
        source_order,
        reference,
        mode,
        lines,
        metadata,
        notes: direct_text(node, "NARRATION").unwrap_or_default(),
    })
}

fn counted_quantity(node: Node<'_, '_>, unit: &UnitMeasure) -> Result<i64> {
    let quantity = unit.quantity(&invoice_field(node, "ACTUALQTY")?)?;
    if quantity < 0 || quantity % 10_i64.pow(u32::from(3 - unit.precision)) != 0 {
        return Err(Error::Validation(
            "Counted quantity must be zero or positive and match the stock unit precision".into(),
        ));
    }
    if let Some(value) = direct_text(node, "BILLEDQTY").filter(|value| !value.is_empty()) {
        let billed = unit.quantity(&value)?;
        if billed != 0 && billed != quantity {
            return Err(Error::Validation("Stock count quantities disagree".into()));
        }
    }
    let priced = direct_text(node, "RATE")
        .filter(|value| !value.is_empty())
        .map(|value| {
            let number = value.split('/').next().unwrap_or_default().trim();
            Decimal::from_str(number)
                .map(|rate| rate != Decimal::ZERO)
                .map_err(|_| Error::Validation("Stock count rate is invalid".into()))
        })
        .transpose()?
        .unwrap_or(false);
    if priced || xml_item_amount(node)? != 0 {
        return Err(Error::Validation(
            "Stock counts with separately recorded values require review".into(),
        ));
    }
    Ok(quantity)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CountMatch {
    item: i64,
    location: i64,
    batch: Option<i64>,
    book: i64,
    counted: i64,
}

pub(super) fn write(
    conn: &Connection,
    company: i64,
    source: &ImportedCount,
    created_batches: &mut HashMap<(i64, String), i64>,
) -> Result<Vec<CountMatch>> {
    let mut lines = Vec::with_capacity(source.lines.len());
    for line in &source.lines {
        let item = resolve_imported_item(
            conn,
            company,
            &line.item,
            line.existing_id,
            &line.unit,
            line.precision,
        )?;
        let (location, batch) =
            resolve_imported_allocation(conn, company, item, &line.allocation, created_batches)?;
        lines.push((
            location,
            CountLine {
                item_id: item,
                batch_id: batch,
                counted_quantity: line.allocation.quantity,
            },
        ));
    }
    let plan = prepare_stock_count(
        conn,
        company,
        CountInput {
            date: &source.date,
            reference: &source.reference,
            notes: &source.notes,
            mode: source.mode,
            lines: &lines,
        },
    )?;
    let count = write_stock_count(conn, company, &plan, true)?;
    save_metadata(conn, company, "stockcount", count, &source.metadata)?;
    Ok(plan
        .lines()
        .iter()
        .map(|line| CountMatch {
            item: line.item_id,
            location: line.location_id,
            batch: line.batch_id,
            book: line.book_quantity,
            counted: line.counted_quantity,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::StockCountMode;
    use crate::{Category, Store};

    fn envelope(vouchers: &str) -> String {
        format!("<ENVELOPE><BODY><DATA><MESSAGE>{vouchers}</MESSAGE></DATA></BODY></ENVELOPE>")
    }

    fn invoice(
        kind: &str,
        date: &str,
        reference: &str,
        party: &str,
        trade: &str,
        quantity: i64,
    ) -> String {
        let inventory = quantity * if kind == "Purchase" { -10 } else { 10 };
        let party_total = -inventory;
        format!(
            "<VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME>
              <VOUCHERNUMBER>{reference}</VOUCHERNUMBER><PARTYLEDGERNAME>{party}</PARTYLEDGERNAME>
              <ISINVOICE>Yes</ISINVOICE>
              <LEDGERENTRIES.LIST><LEDGERNAME>{party}</LEDGERNAME><AMOUNT>{party_total}</AMOUNT>
                <BILLALLOCATIONS.LIST><NAME>{reference}</NAME><BILLTYPE>New Ref</BILLTYPE><AMOUNT>{party_total}</AMOUNT></BILLALLOCATIONS.LIST>
              </LEDGERENTRIES.LIST>
              <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>{quantity} pcs</ACTUALQTY>
                <BILLEDQTY>{quantity} pcs</BILLEDQTY><RATE>10/pcs</RATE><AMOUNT>{inventory}</AMOUNT>
                <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>{trade}</LEDGERNAME><AMOUNT>{inventory}</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
              </ALLINVENTORYENTRIES.LIST></VOUCHER>"
        )
    }

    fn count(date: &str, reference: &str, quantity: i64, allocations: &str) -> String {
        format!(
            "<VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>Physical Stock</VOUCHERTYPENAME>
               <VOUCHERNUMBER>{reference}</VOUCHERNUMBER><NARRATION>Warehouse audit</NARRATION>
               <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME>
                <ACTUALQTY>{quantity} pcs</ACTUALQTY>{allocations}</ALLINVENTORYENTRIES.LIST>
             </VOUCHER>"
        )
    }

    fn fixture() -> (Store, i64, i64, i64) {
        let store = Store::memory().unwrap();
        let company = store.create_company("Imported counts").unwrap();
        for (name, kind) in [
            ("Supplier", Category::Liability),
            ("Purchases", Category::Expense),
            ("Customer", Category::Asset),
            ("Sales", Category::Income),
        ] {
            store.create_ledger(company, name, kind).unwrap();
        }
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        store
            .adjust_stock_at(company, item, location, "2026-04-01", 1000, "Opening")
            .unwrap();
        (store, company, item, location)
    }

    #[test]
    fn import_count_reconciles_each_location_in_the_same_day_voucher_sequence() {
        let (mut store, company, item, _) = fixture();
        let west = store.create_stock_location(company, "West").unwrap();
        let batch = store
            .create_stock_batch(company, item, "Lot W", None, None)
            .unwrap();
        store
            .adjust_stock_batch(
                company,
                item,
                west,
                Some(batch),
                "2026-04-01",
                8000,
                "Opening batch",
            )
            .unwrap();
        let xml = envelope(
            &(invoice("Purchase", "20260402", "PUR", "Supplier", "Purchases", 2)
                + &count(
                    "20260402",
                    "COUNT-1",
                    5,
                    r#"
                <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>5 pcs</ACTUALQTY></BATCHALLOCATIONS.LIST>
                <BATCHALLOCATIONS.LIST><GODOWNNAME>West</GODOWNNAME><BATCHNAME>Lot W</BATCHNAME><ACTUALQTY>0 pcs</ACTUALQTY></BATCHALLOCATIONS.LIST>
            "#,
                )
                + &invoice("Sales", "20260402", "SOLD", "Customer", "Sales", 1)),
        );
        let no_mode = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(!no_mode.can_apply());
        assert!(no_mode
            .issues()
            .iter()
            .any(|issue| issue.contains("Select how physical stock")));
        let plan = store
            .preview_xml_import_batch_with_stock_counts(
                company,
                &[xml.as_bytes()],
                None,
                Some(StockCountMode::Reconcile),
            )
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert_eq!(plan.stock_count_count(), 1);
        assert!(store.stock_counts(company).unwrap().is_empty());
        assert_eq!(store.apply_xml_import(&plan).unwrap().stock_counts, 1);
        let saved = store.stock_counts(company).unwrap().remove(0);
        assert_eq!(saved.location, "Main, West");
        assert_eq!(saved.mode, StockCountMode::Reconcile);
        let lines = store.stock_count_lines(company, saved.id).unwrap();
        assert_eq!(
            (
                lines[0].location.as_str(),
                lines[0].book_quantity,
                lines[0].counted_quantity
            ),
            ("Main", 3000, 5000)
        );
        assert_eq!(
            (
                lines[1].location.as_str(),
                lines[1].book_quantity,
                lines[1].counted_quantity
            ),
            ("West", 8000, 0)
        );
        assert!(lines.iter().all(|line| line.adjustment_id.is_some()));
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            4000
        );
        assert_eq!(store.vouchers(company).unwrap().len(), 2);
    }

    #[test]
    fn observation_import_rechecks_book_quantities_and_preserves_variance_without_adjusting_stock()
    {
        let (mut store, company, item, location) = fixture();
        let xml = envelope(&count("20260403", "COUNT-1", 5, ""));
        let preview = |store: &Store, xml: &str| {
            let plan = store
                .preview_xml_import_batch_with_stock_counts(
                    company,
                    &[xml.as_bytes()],
                    None,
                    Some(StockCountMode::Observe),
                )
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            plan
        };
        let plan = preview(&store, &xml);
        store
            .adjust_stock_at(
                company,
                item,
                location,
                "2026-04-02",
                2000,
                "Earlier delivery",
            )
            .unwrap();
        let err = store.apply_xml_import(&plan).unwrap_err();
        assert!(err.to_string().contains("Stock count book quantities"));
        assert!(store.stock_counts(company).unwrap().is_empty());
        let plan = preview(&store, &xml);
        assert_eq!(store.apply_xml_import(&plan).unwrap().stock_counts, 1);
        let saved = store.stock_counts(company).unwrap().remove(0);
        assert_eq!(saved.mode, StockCountMode::Observe);
        let line = store
            .stock_count_lines(company, saved.id)
            .unwrap()
            .remove(0);
        assert_eq!(
            (
                line.book_quantity,
                line.counted_quantity,
                line.adjustment_id
            ),
            (3000, 5000, None)
        );
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            3000
        );
        let duplicate = store
            .preview_xml_import_batch_with_stock_counts(
                company,
                &[xml.as_bytes()],
                None,
                Some(StockCountMode::Observe),
            )
            .unwrap();
        assert!(!duplicate.can_apply());
        let repeat = preview(&store, &envelope(&count("20260404", "COUNT-1", 3, "")));
        store.apply_xml_import(&repeat).unwrap();
        assert_eq!(store.stock_counts(company).unwrap().len(), 2);
    }

    #[test]
    fn billed_stock_return_in_the_same_batch_contributes_to_the_count_book_quantity() {
        let (mut store, company, item, location) = fixture();
        store
            .adjust_stock_at(
                company,
                item,
                location,
                "2026-04-01",
                7000,
                "Additional opening stock",
            )
            .unwrap();
        let returned = r#"<VOUCHER><DATE>20260403</DATE><VOUCHERTYPENAME>Credit Note</VOUCHERTYPENAME>
          <VOUCHERNUMBER>RET</VOUCHERNUMBER><PARTYLEDGERNAME>Customer</PARTYLEDGERNAME><ISINVOICE>Yes</ISINVOICE>
          <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>10</AMOUNT>
            <BILLALLOCATIONS.LIST><NAME>SOLD</NAME><BILLTYPE>Agst Ref</BILLTYPE><AMOUNT>10</AMOUNT></BILLALLOCATIONS.LIST>
          </LEDGERENTRIES.LIST>
          <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>-10</AMOUNT></LEDGERENTRIES.LIST>
          <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>1 pcs</ACTUALQTY>
            <BILLEDQTY>1 pcs</BILLEDQTY><RATE>10/pcs</RATE><AMOUNT>-10</AMOUNT>
            <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>-10</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
          </ALLINVENTORYENTRIES.LIST>
        </VOUCHER>"#;
        let xml = envelope(
            &(invoice("Sales", "20260402", "SOLD", "Customer", "Sales", 2)
                + returned
                + &count("20260403", "COUNT-AFTER-RETURN", 5, "")),
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
        store.apply_xml_import(&plan).unwrap();
        let saved = store.stock_counts(company).unwrap().remove(0);
        assert_eq!(
            store.stock_count_lines(company, saved.id).unwrap()[0].book_quantity,
            7000
        );
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            5000
        );
    }
}
