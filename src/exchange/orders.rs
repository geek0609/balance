//! Import order documents and their delivery schedules without posting vouchers.
use super::*;
use crate::orders::{OrderDraft, OrderLine};

#[derive(Debug, Clone)]
pub(super) struct ImportedOrder {
    pub kind: InvoiceKind,
    pub date: String,
    pub reference: String,
    pub party: String,
    party_id: Option<i64>,
    expected_on: Option<String>,
    notes: String,
    items: Vec<OrderItem>,
    voucher_number: String,
    metadata: SourceMetadata,
}

#[derive(Debug, Clone)]
struct OrderItem {
    item: String,
    existing_id: Option<i64>,
    unit: String,
    precision: u8,
    quantity: i64,
    unit_price: i64,
    amount: i128,
    lots: Vec<DeliveryLot>,
}

#[derive(Debug, Clone)]
struct DeliveryLot {
    date: Option<String>,
    quantity: i64,
    location: Option<String>,
    batch: Option<String>,
}

pub(super) fn kind(value: &str) -> Option<InvoiceKind> {
    match normalized(value).as_str() {
        "sales order" => Some(InvoiceKind::Sale),
        "purchase order" => Some(InvoiceKind::Purchase),
        _ => None,
    }
}

pub(super) fn source_ref(value: Option<String>) -> Option<String> {
    stock_notes::tracking(value)
}

pub(super) fn due_date(node: Node<'_, '_>) -> Result<Option<String>> {
    source_ref(direct_text(node, "ORDERDUEDATE"))
        .filter(|value| value != "0")
        .map(|value| xml_date(&value))
        .transpose()
}

fn collect_reference(reference: &mut Option<String>, value: Option<String>) -> Result<()> {
    if let Some(value) = source_ref(value) {
        if reference
            .as_ref()
            .is_some_and(|current| normalized(current) != normalized(&value))
        {
            return Err(Error::Validation(
                "A document can reference one order per party and type".into(),
            ));
        }
        if reference.is_none() {
            *reference = Some(value);
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct InvoiceOrder {
    pub reference: String,
    dates: Vec<Vec<Option<String>>>,
}

#[derive(Default)]
struct OrderFields {
    reference: Option<String>,
    expected_on: Option<String>,
}

impl OrderFields {
    fn read(node: Node<'_, '_>, parent: &Self) -> Result<Self> {
        let number = direct_text(node, "ORDERNO").filter(|value| !value.is_empty());
        let alias = direct_text(node, "BASICORDERREF").filter(|value| !value.is_empty());
        let mut reference = source_ref(number.clone());
        collect_reference(&mut reference, alias.clone())?;
        if number.is_none() && alias.is_none() {
            reference = parent.reference.clone();
        }
        let explicit_date = due_date(node)?;
        if reference.is_none() && explicit_date.is_some() {
            return Err(Error::Validation(
                "Invoice order due date requires an order reference".into(),
            ));
        }
        let expected_on = if reference.is_some() {
            explicit_date.or_else(|| parent.expected_on.clone())
        } else {
            None
        };
        Ok(Self {
            reference,
            expected_on,
        })
    }
}

pub(super) fn invoice_order(
    node: Node<'_, '_>,
    inventory: &[(Node<'_, '_>, Option<usize>)],
) -> Result<Option<InvoiceOrder>> {
    let header = OrderFields::read(node, &OrderFields::default())?;
    if inventory.is_empty() && header.reference.is_some() {
        return Err(Error::Validation(
            "An order reference requires an inventory invoice".into(),
        ));
    }
    let mut reference = header.reference.clone();
    let mut has_unlinked = false;
    let mut dates = Vec::new();
    for &(item, _) in inventory {
        let item_fields = OrderFields::read(item, &header)?;
        let mut allocations = Vec::new();
        for batch in item.children().filter(|child| {
            child.is_element()
                && local_name(*child).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
                && meaningful(*child)
        }) {
            allocations.push(OrderFields::read(batch, &item_fields)?);
        }
        if allocations.is_empty() {
            allocations.push(item_fields);
        }
        let mut allocation_dates = Vec::new();
        for fields in allocations {
            has_unlinked |= fields.reference.is_none();
            collect_reference(&mut reference, fields.reference)?;
            allocation_dates.push(fields.expected_on);
        }
        dates.push(allocation_dates);
    }
    if has_unlinked && reference.is_some() {
        return Err(Error::Validation(
            "Every inventory allocation on this invoice must reference the same order".into(),
        ));
    }
    Ok(reference.map(|reference| InvoiceOrder { reference, dates }))
}

impl InvoiceOrder {
    pub(super) fn validate(
        &self,
        conn: &Connection,
        company: i64,
        draft: &InvoiceDraft,
        source: &ImportedInvoice,
    ) -> Result<i64> {
        let order = linked_order(
            conn,
            company,
            draft.party_ledger_id,
            draft.kind,
            &self.reference,
        )?;
        let mut requested = Vec::new();
        if self.dates.len() != source.lines.len() || draft.lines.len() != source.lines.len() {
            return Err(Error::Validation(
                "Invoice order lines do not reconcile".into(),
            ));
        }
        for ((line, imported), dates) in draft.lines.iter().zip(&source.lines).zip(&self.dates) {
            if dates.len() != imported.allocations.len() {
                return Err(Error::Validation(
                    "Invoice order allocations do not reconcile".into(),
                ));
            }
            for (allocation, date) in imported.allocations.iter().zip(dates) {
                if allocation.tracking.is_none() && allocation.quantity > 0 {
                    requested.push((line.item_id, allocation.quantity, date.as_deref()));
                } else if let Some(date) = date {
                    let scheduled: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM order_schedule_progress
                         WHERE order_id = ?1 AND item_id = ?2 AND expected_on = ?3)",
                        params![order, line.item_id, date],
                        |row| row.get(0),
                    )?;
                    if !scheduled {
                        return Err(Error::Validation(
                            "Tracked invoice due date does not match an order lot".into(),
                        ));
                    }
                }
            }
        }
        let date = parse_date(&draft.date)?;
        crate::orders::validate_order_invoice_source(
            conn,
            company,
            order,
            date,
            draft.lines.iter().map(|line| line.item_id),
        )?;
        if !requested.is_empty() {
            crate::orders::check_fulfilment(
                conn,
                company,
                order,
                draft.kind,
                draft.party_ledger_id,
                date,
                crate::orders::FulfilmentChange::NewDocument,
                requested
                    .iter()
                    .map(|&(item, quantity, _)| (item, quantity)),
            )?;
            check_delivery_lots(conn, order, requested.into_iter())?;
        }
        Ok(order)
    }
}

pub(super) fn has_line_order_fields(node: Node<'_, '_>) -> bool {
    ["ORDERNO", "BASICORDERREF", "ORDERDUEDATE"]
        .into_iter()
        .any(|field| source_ref(direct_text(node, field)).is_some_and(|value| value != "0"))
}

fn quantity(node: Node<'_, '_>, unit: &UnitMeasure, kind: InvoiceKind) -> Result<i64> {
    let value = ["ACTUALQTY", "BILLEDQTY"]
        .into_iter()
        .find_map(|field| direct_text(node, field).filter(|value| !value.is_empty()))
        .ok_or_else(|| Error::Validation("Order quantity is missing".into()))?;
    let signed = unit.quantity(&value)?;
    let quantity = signed
        .checked_abs()
        .filter(|&qty| qty > 0)
        .ok_or_else(|| Error::Validation("Order quantity must be positive".into()))?;
    if (kind == InvoiceKind::Purchase && signed < 0)
        || quantity % 10_i64.pow(u32::from(3 - unit.precision)) != 0
    {
        return Err(Error::Validation(
            "Order quantity has an invalid direction or precision".into(),
        ));
    }
    if let Some(value) = direct_text(node, "BILLEDQTY").filter(|value| !value.is_empty()) {
        let billed = unit.quantity(&value)?;
        if billed != 0 && billed != signed {
            return Err(Error::Validation("Order quantities disagree".into()));
        }
    }
    Ok(quantity)
}

pub(super) fn parse(
    node: Node<'_, '_>,
    kind: InvoiceKind,
    label: &str,
    metadata: SourceMetadata,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    stock: &InvoiceStockItems,
    issues: &mut Vec<String>,
) -> Result<ImportedOrder> {
    check_fields(
        node,
        &[
            "DATE",
            "VOUCHERTYPENAME",
            "VOUCHERNUMBER",
            "PARTYLEDGERNAME",
            "NARRATION",
            "ORDERNO",
            "BASICORDERREF",
            "ORDERDUEDATE",
            "ISCANCELLED",
            "ISOPTIONAL",
            "ISINVOICE",
            "PERSISTEDVIEW",
            "OBJVIEW",
            "ALLINVENTORYENTRIES.LIST",
            "INVENTORYENTRIES.LIST",
            "ALLLEDGERENTRIES.LIST",
            "LEDGERENTRIES.LIST",
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
                "Cancelled and optional orders require a separate import workflow".into(),
            ));
        }
    }
    let voucher_number = invoice_field(node, "VOUCHERNUMBER")?;
    let date = xml_date(&invoice_field(node, "DATE")?)?;
    let expected_on = due_date(node)?;
    let party = invoice_field(node, "PARTYLEDGERNAME")?;
    let (party_id, category) = ledgers
        .get(&normalized(&party))
        .ok_or_else(|| Error::Validation(format!("Party {party} is unavailable")))?;
    if !matches!(category, Category::Asset | Category::Liability) {
        return Err(Error::Validation(
            "Order party must be an asset or liability ledger".into(),
        ));
    }
    let mut reference = source_ref(direct_text(node, "ORDERNO"));
    collect_reference(&mut reference, direct_text(node, "BASICORDERREF"))?;
    let mut items = BTreeMap::<String, OrderItem>::new();
    for entry in node
        .children()
        .filter(|row| row.is_element() && meaningful(*row))
    {
        match local_name(entry).to_ascii_uppercase().as_str() {
            "LEDGERENTRIES.LIST" | "ALLLEDGERENTRIES.LIST" => {
                invoice_list_fields(
                    entry,
                    &["LEDGERNAME", "AMOUNT", "ISDEEMEDPOSITIVE", "ISPARTYLEDGER"],
                    label,
                    issues,
                );
                xml_item_amount(entry)?;
            }
            "ALLINVENTORYENTRIES.LIST" | "INVENTORYENTRIES.LIST" => {
                invoice_list_fields(
                    entry,
                    &[
                        "STOCKITEMNAME",
                        "ACTUALQTY",
                        "BILLEDQTY",
                        "RATE",
                        "AMOUNT",
                        "ISDEEMEDPOSITIVE",
                        "ORDERNO",
                        "ORDERDUEDATE",
                        "BATCHALLOCATIONS.LIST",
                        "GODOWNNAME",
                        "BATCHNAME",
                        "ACCOUNTINGALLOCATIONS.LIST",
                    ],
                    label,
                    issues,
                );
                let item = invoice_field(entry, "STOCKITEMNAME")?;
                let key = normalized(&item);
                let (existing_id, unit) = stock.get(&key).ok_or_else(|| {
                    Error::Validation(format!("Stock item {item} is unavailable"))
                })?;
                let qty = quantity(entry, unit, kind)?;
                let rate = invoice_field(entry, "RATE")?;
                let (price, suffix) = rate.split_once('/').ok_or_else(|| {
                    Error::Validation("Order rate must specify the stock unit".into())
                })?;
                if unit.rate_basis(suffix)?.is_some() {
                    return Err(Error::Validation(
                        "Order rates in a larger compound unit require a separate pricing workflow"
                            .into(),
                    ));
                }
                let price = parse_amount(price)?;
                let expected_amount = crate::inventory::line_amount(qty, price)?;
                if direct_text(entry, "AMOUNT").is_some_and(|amount| !amount.is_empty())
                    && xml_item_amount(entry)?.checked_abs() != Some(expected_amount)
                {
                    return Err(Error::Validation(
                        "Order amount differs from its quantity and rate".into(),
                    ));
                }
                for account in entry.children().filter(|c| {
                    c.is_element()
                        && local_name(*c).eq_ignore_ascii_case("ACCOUNTINGALLOCATIONS.LIST")
                        && meaningful(*c)
                }) {
                    invoice_list_fields(
                        account,
                        &["LEDGERNAME", "AMOUNT", "ISDEEMEDPOSITIVE"],
                        label,
                        issues,
                    );
                    xml_item_amount(account)?;
                }
                collect_reference(&mut reference, direct_text(entry, "ORDERNO"))?;
                let item_date = due_date(entry)?;
                let mut lots = Vec::new();
                for lot in entry.children().filter(|c| {
                    c.is_element()
                        && local_name(*c).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
                        && meaningful(*c)
                }) {
                    invoice_list_fields(
                        lot,
                        &[
                            "ACTUALQTY",
                            "BILLEDQTY",
                            "ORDERNO",
                            "ORDERDUEDATE",
                            "AMOUNT",
                            "GODOWNNAME",
                            "BATCHNAME",
                        ],
                        label,
                        issues,
                    );
                    collect_reference(&mut reference, direct_text(lot, "ORDERNO"))?;
                    let q = quantity(lot, unit, kind)?;
                    if direct_text(lot, "AMOUNT").is_some_and(|value| !value.is_empty())
                        && xml_item_amount(lot)?.checked_abs()
                            != Some(crate::inventory::line_amount(q, price)?)
                    {
                        return Err(Error::Validation(
                            "Order lot amount differs from its quantity and rate".into(),
                        ));
                    }
                    lots.push(DeliveryLot {
                        date: due_date(lot)?.or_else(|| item_date.clone()),
                        quantity: q,
                        location: source_ref(
                            direct_text(lot, "GODOWNNAME")
                                .or_else(|| direct_text(entry, "GODOWNNAME")),
                        ),
                        batch: source_ref(
                            direct_text(lot, "BATCHNAME")
                                .or_else(|| direct_text(entry, "BATCHNAME")),
                        ),
                    });
                }
                if lots.is_empty() {
                    lots.push(DeliveryLot {
                        date: item_date,
                        quantity: qty,
                        location: source_ref(direct_text(entry, "GODOWNNAME")),
                        batch: source_ref(direct_text(entry, "BATCHNAME")),
                    });
                }
                if lots
                    .iter()
                    .map(|lot| i128::from(lot.quantity))
                    .sum::<i128>()
                    != i128::from(qty)
                {
                    return Err(Error::Validation(
                        "Order schedule quantities do not match the item quantity".into(),
                    ));
                }
                let order_item = items.entry(key).or_insert_with(|| OrderItem {
                    item,
                    existing_id: *existing_id,
                    unit: unit.symbol.clone(),
                    precision: unit.precision,
                    quantity: 0,
                    unit_price: price,
                    amount: 0,
                    lots: Vec::new(),
                });
                if order_item.unit_price != price {
                    return Err(Error::Validation(
                        "Several prices for the same order item require separate order lines"
                            .into(),
                    ));
                }
                order_item.quantity = order_item
                    .quantity
                    .checked_add(qty)
                    .ok_or_else(|| Error::Validation("Order quantity is too large".into()))?;
                order_item.amount += i128::from(expected_amount);
                order_item.lots.extend(lots);
            }
            _ => {}
        }
    }
    if items.is_empty() {
        return Err(Error::Validation("Order has no inventory items".into()));
    }
    for item in items.values() {
        if i128::from(crate::inventory::line_amount(
            item.quantity,
            item.unit_price,
        )?) != item.amount
        {
            return Err(Error::Validation(
                "Order item amounts do not reconcile when combined".into(),
            ));
        }
    }
    Ok(ImportedOrder {
        kind,
        date,
        reference: reference.unwrap_or_else(|| voucher_number.clone()),
        party,
        party_id: *party_id,
        expected_on,
        notes: direct_text(node, "NARRATION").unwrap_or_default(),
        items: items.into_values().collect(),
        voucher_number,
        metadata,
    })
}

pub(super) fn write(conn: &Connection, company: i64, order: &ImportedOrder) -> Result<()> {
    let party = find_ledger(conn, company, &order.party)?
        .ok_or_else(|| Error::Validation(format!("Party {} is missing", order.party)))?;
    if order.party_id.is_some_and(|expected| expected != party) {
        return Err(Error::Validation(format!(
            "Party {} changed since preview",
            order.party
        )));
    }
    let lines = order
        .items
        .iter()
        .map(|item| {
            Ok(OrderLine {
                item_id: resolve_imported_item(
                    conn,
                    company,
                    &item.item,
                    item.existing_id,
                    &item.unit,
                    item.precision,
                )?,
                quantity: item.quantity,
                unit_price: item.unit_price,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let id = crate::orders::insert_order(
        conn,
        company,
        &OrderDraft {
            kind: order.kind,
            party_ledger_id: party,
            reference: order.reference.clone(),
            date: order.date.clone(),
            expected_on: order.expected_on.clone(),
            notes: order.notes.clone(),
            lines: lines.clone(),
        },
    )?;
    conn.execute(
        "INSERT INTO imported_order_numbers(order_id, voucher_number, reference_key) VALUES (?1, ?2, ?3)",
        params![id, order.voucher_number, normalized(&order.reference)],
    )?;
    save_metadata(conn, company, "order", id, &order.metadata)?;
    for (line, item) in lines.into_iter().zip(&order.items) {
        if item.lots.len() == 1
            && item
                .lots
                .iter()
                .all(|lot| lot.date.is_none() && lot.location.is_none() && lot.batch.is_none())
        {
            continue;
        }
        let line_id: i64 = conn.query_row(
            "SELECT id FROM order_lines WHERE order_id = ?1 AND item_id = ?2",
            params![id, line.item_id],
            |row| row.get(0),
        )?;
        for lot in &item.lots {
            if lot.date.as_ref().is_some_and(|due| due < &order.date) {
                return Err(Error::Validation(
                    "Order lot date cannot precede the order date".into(),
                ));
            }
            conn.execute("INSERT INTO order_delivery_lots(order_line_id, expected_on, quantity, source_location, source_batch)
                          VALUES (?1, ?2, ?3, ?4, ?5)", params![line_id, lot.date, lot.quantity, lot.location, lot.batch])?;
        }
    }
    Ok(())
}

pub(super) fn linked_order(
    conn: &Connection,
    company: i64,
    party: i64,
    kind: InvoiceKind,
    reference: &str,
) -> Result<i64> {
    let mut stmt = conn.prepare(
        "SELECT o.id FROM orders o LEFT JOIN imported_order_numbers src ON src.order_id = o.id
         WHERE o.company_id = ?1 AND o.party_ledger_id = ?2 AND o.kind = ?3
           AND (o.reference_key = ?4 OR src.reference_key = ?4)",
    )?;
    let mut rows = stmt.query(params![
        company,
        party,
        kind.as_str(),
        normalized(reference)
    ])?;
    let id = rows
        .next()?
        .ok_or_else(|| {
            Error::Validation(format!(
                "Order {reference} is unavailable for this document"
            ))
        })?
        .get(0)?;
    if rows.next()?.is_some() {
        return Err(Error::Validation(format!(
            "Order reference {reference} is ambiguous"
        )));
    }
    Ok(id)
}

pub(super) fn check_delivery_lots<'a>(
    conn: &Connection,
    order: i64,
    lines: impl Iterator<Item = (i64, i64, Option<&'a str>)>,
) -> Result<()> {
    let requests: Vec<_> = lines.collect();
    let checked: HashSet<_> = requests
        .iter()
        .filter_map(|(item, _, date)| date.map(|_| *item))
        .collect();
    if checked.is_empty() {
        return Ok(());
    }
    let mut lot_queries = conn.prepare(
        "SELECT item_id, expected_on, remaining FROM order_schedule_progress
         WHERE order_id = ?1 AND remaining > 0 ORDER BY expected_on IS NULL, expected_on, lot_id",
    )?;
    let mut lots = HashMap::<i64, VecDeque<(Option<String>, i64)>>::new();
    let mut rows = lot_queries.query([order])?;
    while let Some(row) = rows.next()? {
        lots.entry(row.get(0)?)
            .or_default()
            .push_back((row.get(1)?, row.get(2)?));
    }
    for (item, mut quantity, expected) in requests {
        if !checked.contains(&item) {
            continue;
        }
        let pending = lots.entry(item).or_default();
        while quantity > 0 {
            let Some((date, available)) = pending.front_mut() else {
                return Err(Error::Validation(
                    "Stock note exceeds the scheduled order quantity".into(),
                ));
            };
            if expected.is_some() && expected != date.as_deref() {
                return Err(Error::Validation(
                    "Stock note due date does not match the next open order lot".into(),
                ));
            }
            let consumed = quantity.min(*available);
            quantity -= consumed;
            *available -= consumed;
            if *available == 0 {
                pending.pop_front();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(vouchers: &str) -> Vec<u8> {
        format!("<ENVELOPE><BODY><DATA><MESSAGE>{vouchers}</MESSAGE></DATA></BODY></ENVELOPE>")
            .into_bytes()
    }

    #[test]
    fn a_later_receipt_finds_the_source_order_reference_after_an_amendment() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Supplier orders").unwrap();
        store
            .create_ledger(company, "Supplier", Category::Liability)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let order = envelope(
            r#"<VOUCHER><DATE>20260401</DATE><VOUCHERTYPENAME>Purchase Order</VOUCHERTYPENAME>
          <VOUCHERNUMBER>V-13</VOUCHERNUMBER><PARTYLEDGERNAME>Supplier</PARTYLEDGERNAME>
          <BASICORDERREF>PO-EXT</BASICORDERREF><ORDERDUEDATE>20260412</ORDERDUEDATE>
          <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
            <RATE>10/pcs</RATE><AMOUNT>-20</AMOUNT><GODOWNNAME>Main</GODOWNNAME>
            <BATCHALLOCATIONS.LIST><ACTUALQTY>2 pcs</ACTUALQTY><BATCHNAME>Planned lot</BATCHNAME></BATCHALLOCATIONS.LIST>
          </INVENTORYENTRIES.LIST></VOUCHER>"#,
        );
        let plan = store.preview_xml_import(company, &order).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let id = store.orders(company).unwrap()[0].id;
        let detail = store.order_detail(company, id).unwrap();
        assert_eq!(
            detail.delivery_lots[0].source_batch.as_deref(),
            Some("Planned lot")
        );
        assert_eq!(
            detail.delivery_lots[0].source_location.as_deref(),
            Some("Main")
        );
        store
            .amend_order(
                company,
                id,
                &crate::orders::OrderAmendmentDraft {
                    base_revision: detail.summary.revision,
                    reference: "LOCAL-PO".into(),
                    expected_on: detail.summary.expected_on,
                    notes: String::new(),
                    lines: detail.lines.into_iter().map(|part| part.line).collect(),
                    reason: "Local reference".into(),
                },
            )
            .unwrap();
        let receipt = envelope(
            r#"<VOUCHER><DATE>20260406</DATE><VOUCHERTYPENAME>Receipt Note</VOUCHERTYPENAME>
          <VOUCHERNUMBER>RECEIVED</VOUCHERNUMBER><PARTYLEDGERNAME>Supplier</PARTYLEDGERNAME>
          <ORDERNO>po-ext</ORDERNO><ORDERDUEDATE>20260412</ORDERDUEDATE>
          <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY>
            <RATE>8/pcs</RATE></INVENTORYENTRIES.LIST></VOUCHER>"#,
        );
        let plan = store.preview_xml_import(company, &receipt).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        assert_eq!(store.stock_notes(company, Some(id)).unwrap().len(), 1);
        assert_eq!(
            store.order_detail(company, id).unwrap().lines[0].fulfilled,
            2000
        );
        assert!(store.vouchers(company).unwrap().is_empty());
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            2000
        );
    }

    #[test]
    fn xml_order_schedule_and_linked_stock_agree_after_import_and_reversal() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Order migration").unwrap();
        store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        store
            .adjust_stock_at(
                company,
                item,
                location,
                "2026-04-01",
                5000,
                "Opening quantity",
            )
            .unwrap();
        let order = r#"<VOUCHER><DATE>20260401</DATE><VOUCHERTYPENAME>Sales Order</VOUCHERTYPENAME>
          <VOUCHERNUMBER>V-7</VOUCHERNUMBER><PARTYLEDGERNAME>Customer</PARTYLEDGERNAME>
          <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>-5 pcs</ACTUALQTY>
            <RATE>10/pcs</RATE><AMOUNT>50</AMOUNT><ORDERNO>CUSTOMER-17</ORDERNO>
            <BATCHALLOCATIONS.LIST><ACTUALQTY>-2 pcs</ACTUALQTY><ORDERDUEDATE>20260405</ORDERDUEDATE></BATCHALLOCATIONS.LIST>
            <BATCHALLOCATIONS.LIST><ACTUALQTY>-3 pcs</ACTUALQTY><ORDERDUEDATE>20260412</ORDERDUEDATE></BATCHALLOCATIONS.LIST>
          </ALLINVENTORYENTRIES.LIST></VOUCHER>"#;
        let note = r#"<VOUCHER><DATE>20260404</DATE><VOUCHERTYPENAME>Delivery Note</VOUCHERTYPENAME>
          <VOUCHERNUMBER>D-1</VOUCHERNUMBER><PARTYLEDGERNAME>Customer</PARTYLEDGERNAME>
          <INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>-2 pcs</ACTUALQTY>
            <BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>-2 pcs</ACTUALQTY>
              <ORDERNO>customer-17</ORDERNO><ORDERDUEDATE>20260405</ORDERDUEDATE>
              <TRACKINGNUMBER>Dispatch</TRACKINGNUMBER></BATCHALLOCATIONS.LIST></INVENTORYENTRIES.LIST></VOUCHER>"#;
        let invoice = r#"<VOUCHER><DATE>20260406</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
          <VOUCHERNUMBER>BILL</VOUCHERNUMBER><PARTYLEDGERNAME>Customer</PARTYLEDGERNAME><ISINVOICE>Yes</ISINVOICE>
          <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>-20</AMOUNT><BILLALLOCATIONS.LIST>
            <NAME>BILL</NAME><BILLTYPE>New Ref</BILLTYPE><AMOUNT>-20</AMOUNT></BILLALLOCATIONS.LIST></LEDGERENTRIES.LIST>
          <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>-2 pcs</ACTUALQTY><BILLEDQTY>-2 pcs</BILLEDQTY>
            <RATE>10/pcs</RATE><AMOUNT>20</AMOUNT><TRACKINGNUMBER>Dispatch</TRACKINGNUMBER>
            <ORDERNO>CUSTOMER-17</ORDERNO><ORDERDUEDATE>20260405</ORDERDUEDATE>
            <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>20</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
          </ALLINVENTORYENTRIES.LIST></VOUCHER>"#;
        let wrong = note.replace("20260405", "20260412");
        let rejected = store
            .preview_xml_import(company, &envelope(&(wrong + order)))
            .unwrap();
        assert!(rejected
            .issues()
            .iter()
            .any(|issue| issue.contains("next open order lot")));
        assert!(store.orders(company).unwrap().is_empty());
        let bytes = envelope(&[note, invoice, order].concat());
        let plan = store.preview_xml_import(company, &bytes).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert_eq!(
            (
                plan.order_count(),
                plan.stock_note_count(),
                plan.stock_match_count()
            ),
            (1, 1, 1)
        );
        let counts = store.apply_xml_import(&plan).unwrap();
        assert_eq!(
            (
                counts.orders,
                counts.stock_notes,
                counts.stock_matches,
                counts.invoices
            ),
            (1, 1, 1, 1)
        );
        assert!(!store
            .preview_xml_import(company, &bytes)
            .unwrap()
            .can_apply());
        let id = store.orders(company).unwrap()[0].id;
        let summary = store.order_detail(company, id).unwrap();
        assert_eq!(summary.source_voucher_number.as_deref(), Some("V-7"));
        assert_eq!(summary.summary.reference, "CUSTOMER-17");
        assert_eq!(
            (summary.lines[0].fulfilled, summary.lines[0].billed),
            (2000, 2000)
        );
        assert_eq!(
            summary
                .delivery_lots
                .iter()
                .map(|lot| lot.remaining)
                .collect::<Vec<_>>(),
            [0, 3000]
        );
        assert!(store
            .pending_order_schedule(company, None, Some("2026-04-11"))
            .unwrap()
            .is_empty());
        let schedule = store
            .pending_order_schedule(company, None, Some("2026-04-12"))
            .unwrap();
        assert_eq!(schedule[0].remaining, 3000);
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            3000
        );
        let note_id = store.stock_notes(company, Some(id)).unwrap()[0].id;
        let bill = store.invoices(company).unwrap()[0].id;
        store
            .reverse_invoice(company, bill, "2026-04-13", "VOID-BILL", "Reverse bill")
            .unwrap();
        let second_dispatch = note
            .replace("D-1", "D-2")
            .replace("20260404", "20260412")
            .replace("20260405", "20260412");
        let second = store
            .preview_xml_import(company, &envelope(&second_dispatch))
            .unwrap();
        assert!(second.can_apply(), "{:?}", second.issues());
        store.apply_xml_import(&second).unwrap();
        let second = store.stock_notes(company, Some(id)).unwrap()[0].id;
        assert_ne!(second, note_id);
        store
            .reverse_stock_note(company, second, "2026-04-14", "Reverse second dispatch")
            .unwrap();
        store
            .reverse_stock_note(company, note_id, "2026-04-14", "Reverse dispatch")
            .unwrap();
        let schedule = store
            .pending_order_schedule(company, Some(InvoiceKind::Sale), None)
            .unwrap();
        assert_eq!(
            schedule.iter().map(|lot| lot.remaining).collect::<Vec<_>>(),
            [2000, 3000]
        );
    }

    #[test]
    fn directly_linked_invoice_xml_uses_the_native_order_and_reservation_path() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Direct order invoices").unwrap();
        let customer = store
            .create_ledger(company, "Customer", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        let item = store.create_stock_item(company, "Part", "pcs").unwrap();
        let location = store.stock_locations(company).unwrap()[0].id;
        store
            .adjust_stock_at(company, item, location, "2026-04-01", 4000, "Opening")
            .unwrap();
        let order = store
            .create_order(
                company,
                &OrderDraft {
                    kind: InvoiceKind::Sale,
                    party_ledger_id: customer,
                    reference: "OPEN-ORDER".into(),
                    date: "2026-04-01".into(),
                    expected_on: Some("2026-04-05".into()),
                    notes: String::new(),
                    lines: vec![OrderLine {
                        item_id: item,
                        quantity: 3000,
                        unit_price: 1000,
                    }],
                },
            )
            .unwrap();
        store
            .reserve_order_stock(
                company,
                order,
                &crate::stock_reservations::StockReservationDraft {
                    item_id: item,
                    location_id: location,
                    batch_id: None,
                    quantity: 3000,
                    reason: "Hold for order".into(),
                },
            )
            .unwrap();
        let invoice = |reference: &str, header: &str, inventory: &str| {
            format!(
            "<VOUCHER><DATE>20260405</DATE><VOUCHERTYPENAME>Sales</VOUCHERTYPENAME><VOUCHERNUMBER>{reference}</VOUCHERNUMBER>
             <PARTYLEDGERNAME>Customer</PARTYLEDGERNAME><ISINVOICE>Yes</ISINVOICE>{header}
             <LEDGERENTRIES.LIST><LEDGERNAME>Customer</LEDGERNAME><AMOUNT>-10</AMOUNT><BILLALLOCATIONS.LIST>
             <NAME>{reference}</NAME><BILLTYPE>New Ref</BILLTYPE><AMOUNT>-10</AMOUNT></BILLALLOCATIONS.LIST></LEDGERENTRIES.LIST>
             <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>1 pcs</ACTUALQTY><BILLEDQTY>1 pcs</BILLEDQTY>
             <RATE>10/pcs</RATE><AMOUNT>10</AMOUNT><ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>10</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
             {inventory}</ALLINVENTORYENTRIES.LIST></VOUCHER>"
        )
        };
        let wrong = invoice(
            "WRONG",
            "<BASICORDERREF>OTHER</BASICORDERREF>",
            "<ORDERNO>OPEN-ORDER</ORDERNO>",
        );
        assert!(!store
            .preview_xml_import(company, &envelope(&wrong))
            .unwrap()
            .can_apply());
        let due = invoice(
            "WRONG",
            "",
            "<ORDERNO>OPEN-ORDER</ORDERNO><ORDERDUEDATE>20260412</ORDERDUEDATE>",
        );
        let preview = store.preview_xml_import(company, &envelope(&due)).unwrap();
        assert!(preview
            .issues()
            .iter()
            .any(|issue| issue.contains("next open order lot")));
        let variants = [
            ("HEADER", "<BASICORDERREF>OPEN-ORDER</BASICORDERREF>", ""),
            (
                "ITEM",
                "",
                "<ORDERNO>OPEN-ORDER</ORDERNO><ORDERDUEDATE>20260405</ORDERDUEDATE>",
            ),
            (
                "BATCH",
                "",
                "<BATCHALLOCATIONS.LIST><GODOWNNAME>Main</GODOWNNAME><ACTUALQTY>1 pcs</ACTUALQTY>
                <BILLEDQTY>1 pcs</BILLEDQTY><AMOUNT>10</AMOUNT><ORDERNO>OPEN-ORDER</ORDERNO>
                <ORDERDUEDATE>20260405</ORDERDUEDATE></BATCHALLOCATIONS.LIST>",
            ),
        ];
        for (count, (reference, header, inventory)) in variants.into_iter().enumerate() {
            let plan = store
                .preview_xml_import(company, &envelope(&invoice(reference, header, inventory)))
                .unwrap();
            assert!(plan.can_apply(), "{:?}", plan.issues());
            store.apply_xml_import(&plan).unwrap();
            let detail = store.order_detail(company, order).unwrap();
            assert_eq!(detail.lines[0].fulfilled, (count as i64 + 1) * 1000);
            assert_eq!(detail.reservations[0].remaining, (2 - count as i64) * 1000);
        }
        assert!(store
            .pending_order_schedule(company, None, None)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            1000
        );
        let original = store
            .invoices(company)
            .unwrap()
            .into_iter()
            .find(|row| row.reference == "ITEM")
            .unwrap();
        assert_eq!(original.order_ids, [order]);
        store
            .reverse_invoice(
                company,
                original.id,
                "2026-04-06",
                "REVERSE",
                "Reopen order",
            )
            .unwrap();
        assert_eq!(
            store.pending_order_schedule(company, None, None).unwrap()[0].remaining,
            1000
        );
        assert_eq!(
            store
                .stock_register(company, item, None, None)
                .unwrap()
                .closing_quantity,
            2000
        );
    }
}
