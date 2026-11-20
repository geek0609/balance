//! Import stock returned before billing, linked to its original stock note by tracking number.
use super::*;
use crate::stock_returns::{write_stock_return, StockReturnSelection};
use std::ops::Range;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS imported_stock_returns(
            id INTEGER PRIMARY KEY,
            company_id INTEGER NOT NULL REFERENCES companies(id),
            kind TEXT NOT NULL CHECK(kind IN ('Sale', 'Purchase')),
            party_ledger_id INTEGER NOT NULL REFERENCES ledgers(id),
            date TEXT NOT NULL,
            reference TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            UNIQUE(company_id, kind, party_ledger_id, date, reference_key)
         );
         CREATE TABLE IF NOT EXISTS imported_stock_return_parts(
            return_id INTEGER PRIMARY KEY REFERENCES stock_returns(id),
            document_id INTEGER NOT NULL REFERENCES imported_stock_returns(id)
         );
         CREATE INDEX IF NOT EXISTS imported_stock_return_document ON imported_stock_return_parts(document_id);",
    )?;
    Ok(())
}

pub(super) fn kind(value: &str) -> Option<InvoiceKind> {
    match normalized(value).as_str() {
        "rejections in" => Some(InvoiceKind::Sale),
        "rejections out" => Some(InvoiceKind::Purchase),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(super) struct ImportedRejection {
    pub date: String,
    pub kind: InvoiceKind,
    pub source_order: usize,
    reference: String,
    party: String,
    party_id: Option<i64>,
    reason: String,
    allocations: Vec<RejectionAllocation>,
    metadata: SourceMetadata,
}

impl ImportedRejection {
    pub(super) fn tracking_keys(&self) -> HashSet<String> {
        self.allocations
            .iter()
            .map(|line| normalized(&line.tracking))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct RejectionAllocation {
    item: String,
    existing_item_id: Option<i64>,
    tracking: String,
    location: Option<String>,
    batch: Option<String>,
    quantity: i64,
    order: orders::OrderFields,
}

pub(super) fn parse(
    node: Node<'_, '_>,
    kind: InvoiceKind,
    source_order: usize,
    label: &str,
    metadata: SourceMetadata,
    ledgers: &HashMap<String, (Option<i64>, Category)>,
    items: &InvoiceStockItems,
    issues: &mut Vec<String>,
) -> Result<ImportedRejection> {
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
            "ISCANCELLED",
            "ISOPTIONAL",
            "ORDERNO",
            "BASICORDERREF",
            "ORDERDUEDATE",
            "TRACKINGNUMBER",
            "TRACKINGNO",
            "VOUCHERTRACKINGNUMBER",
            "VOUCHERTRACKINGNO",
            "VCHTRACKINGNO",
            "ALLINVENTORYENTRIES.LIST",
            "INVENTORYENTRIES.LIST",
            "LEDGERENTRIES.LIST",
            "ALLLEDGERENTRIES.LIST",
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
                "Cancelled and optional stock returns cannot be posted".into(),
            ));
        }
    }
    let party = invoice_field(node, "PARTYLEDGERNAME")?;
    let &(party_id, category) = ledgers
        .get(&normalized(&party))
        .ok_or_else(|| Error::Validation(format!("Party {party} is unavailable")))?;
    if !matches!(category, Category::Asset | Category::Liability) {
        return Err(Error::Validation(
            "The return party must be a customer or supplier ledger".into(),
        ));
    }
    let header_order = orders::OrderFields::read(node, &orders::OrderFields::default())?;
    let header_tracking = stock_notes::tracking_field(node)?;
    let mut allocations = Vec::new();
    for item in node
        .children()
        .filter(|entry| entry.is_element() && meaningful(*entry))
    {
        match local_name(item).to_ascii_uppercase().as_str() {
            "LEDGERENTRIES.LIST" | "ALLLEDGERENTRIES.LIST" => {
                invoice_list_fields(
                    item,
                    &[
                        "LEDGERNAME",
                        "AMOUNT",
                        "ISDEEMEDPOSITIVE",
                        "ISPARTYLEDGER",
                        "ISLASTDEEMEDPOSITIVE",
                    ],
                    label,
                    issues,
                );
                if xml_item_amount(item)? != 0 {
                    return Err(Error::Validation(
                        "A stock-only return cannot post a ledger amount".into(),
                    ));
                }
                continue;
            }
            "ALLINVENTORYENTRIES.LIST" | "INVENTORYENTRIES.LIST" => {}
            _ => continue,
        }
        invoice_list_fields(
            item,
            &[
                "STOCKITEMNAME",
                "ACTUALQTY",
                "BILLEDQTY",
                "RATE",
                "AMOUNT",
                "ISDEEMEDPOSITIVE",
                "GODOWNNAME",
                "BATCHNAME",
                "TRACKINGNUMBER",
                "TRACKINGNO",
                "VOUCHERTRACKINGNUMBER",
                "VOUCHERTRACKINGNO",
                "VCHTRACKINGNO",
                "ORDERNO",
                "BASICORDERREF",
                "ORDERDUEDATE",
                "BATCHALLOCATIONS.LIST",
            ],
            label,
            issues,
        );
        let name = invoice_field(item, "STOCKITEMNAME")?;
        let &(existing_item_id, ref unit) = items
            .get(&normalized(&name))
            .ok_or_else(|| Error::Validation(format!("Stock item {name} is unavailable")))?;
        let actual = unit.quantity(&invoice_field(item, "ACTUALQTY")?)?;
        let expected_total = actual
            .checked_abs()
            .filter(|&qty| qty > 0)
            .ok_or_else(|| Error::Validation("Invalid stock return quantity".into()))?;
        xml_item_amount(item)?;
        let item_order = orders::OrderFields::read(item, &header_order)?;
        let item_tracking = stock_notes::tracking_field(item)?;
        let mut parts = item
            .children()
            .filter(|part| {
                part.is_element()
                    && meaningful(*part)
                    && local_name(*part).eq_ignore_ascii_case("BATCHALLOCATIONS.LIST")
            })
            .collect::<Vec<_>>();
        if parts.is_empty() {
            parts.push(item);
        }
        let mut actual_total = 0_i128;
        for part in parts {
            if part != item {
                invoice_list_fields(
                    part,
                    &[
                        "ACTUALQTY",
                        "BILLEDQTY",
                        "AMOUNT",
                        "GODOWNNAME",
                        "BATCHNAME",
                        "TRACKINGNUMBER",
                        "TRACKINGNO",
                        "VOUCHERTRACKINGNUMBER",
                        "VOUCHERTRACKINGNO",
                        "VCHTRACKINGNO",
                        "ORDERNO",
                        "BASICORDERREF",
                        "ORDERDUEDATE",
                    ],
                    label,
                    issues,
                );
            }
            let part_actual = unit.quantity(&invoice_field(part, "ACTUALQTY")?)?;
            let quantity = part_actual
                .checked_abs()
                .filter(|&qty| qty > 0 && part_actual.signum() == actual.signum())
                .ok_or_else(|| {
                    Error::Validation("Return allocation has an invalid quantity direction".into())
                })?;
            if quantity % 10_i64.pow(u32::from(3 - unit.precision)) != 0 {
                return Err(Error::Validation(
                    "Return quantity exceeds unit precision".into(),
                ));
            }
            xml_item_amount(part)?;
            let tracking = stock_notes::tracking(
                stock_notes::tracking_field(part)?
                    .or(item_tracking.clone())
                    .or(header_tracking.clone()),
            )
            .ok_or_else(|| {
                Error::Validation(
                    "Each stock return allocation needs its original tracking number".into(),
                )
            })?;
            let declared = |field| {
                direct_text(part, field)
                    .filter(|value| !value.is_empty())
                    .or_else(|| direct_text(item, field).filter(|value| !value.is_empty()))
            };
            let order = if part == item {
                item_order.clone()
            } else {
                orders::OrderFields::read(part, &item_order)?
            };
            allocations.push(RejectionAllocation {
                item: name.clone(),
                existing_item_id,
                tracking,
                location: declared("GODOWNNAME"),
                batch: declared("BATCHNAME"),
                quantity,
                order,
            });
            actual_total += i128::from(quantity);
        }
        if actual_total != i128::from(expected_total) {
            return Err(Error::Validation(
                "Return allocation quantities do not match the item".into(),
            ));
        }
    }
    if allocations.is_empty() {
        return Err(Error::Validation(
            "Stock return has no inventory allocations".into(),
        ));
    }
    Ok(ImportedRejection {
        date: xml_date(&invoice_field(node, "DATE")?)?,
        kind,
        source_order,
        reference: invoice_field(node, "VOUCHERNUMBER")?,
        party,
        party_id,
        reason: direct_text(node, "NARRATION")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Imported stock return".into()),
        allocations,
        metadata,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReturnMatch {
    note_line: i64,
    spans: Vec<Range<i64>>,
}

/// Remove positions already included on earlier allocations of this return.
fn unselected(eligible: Vec<Range<i64>>, selected: &[Range<i64>]) -> Vec<Range<i64>> {
    let mut available = Vec::new();
    for span in eligible {
        let mut start = span.start;
        for used in selected {
            if used.end <= start {
                continue;
            }
            if used.start >= span.end {
                break;
            }
            if start < used.start {
                available.push(start..used.start);
            }
            start = start.max(used.end);
        }
        if start < span.end {
            available.push(start..span.end);
        }
    }
    available
}

pub(super) fn write(
    conn: &Connection,
    company: i64,
    source: &ImportedRejection,
) -> Result<Vec<ReturnMatch>> {
    let party = find_ledger(conn, company, &source.party)?
        .filter(|&id| source.party_id.is_none_or(|prior| prior == id))
        .ok_or_else(|| Error::Validation("Return party changed since preview".into()))?;
    let mut planned = BTreeMap::<i64, (i64, Vec<Range<i64>>)>::new();
    let mut allocations = source.allocations.iter().collect::<Vec<_>>();
    allocations.sort_by_key(|part| part.order.expected_on.is_none());
    let mut matches = conn.prepare_cached(
        "SELECT n.id, l.id FROM imported_stock_note_tracking t
         JOIN stock_note_lines l ON l.id = t.note_line_id
         JOIN stock_notes n ON n.id = l.note_id
         LEFT JOIN stock_note_order_sources o ON o.note_line_id = l.id
         WHERE t.reference_key = ?1 AND n.company_id = ?2 AND n.party_ledger_id = ?3 AND n.kind = ?4
           AND n.date <= ?5 AND n.reversed_on IS NULL AND l.item_id = ?6 AND (?7 IS NULL OR o.order_id = ?7)
         ORDER BY n.date, n.id, l.id",
    )?;
    let mut source_lines = HashMap::new();
    for part in allocations {
        let item = find_stock_item(conn, company, &part.item)?
            .filter(|&id| part.existing_item_id.is_none_or(|prior| prior == id))
            .ok_or_else(|| {
                Error::Validation(format!("Return item {} changed since preview", part.item))
            })?;
        let order = part
            .order
            .reference
            .as_deref()
            .map(|name| orders::linked_order(conn, company, party, source.kind, name))
            .transpose()?;
        let rows = matches.query_map(
            params![
                normalized(&part.tracking),
                company,
                party,
                source.kind.as_str(),
                source.date,
                item,
                order
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        let mut candidates = Vec::new();
        let mut total = 0_i128;
        for row in rows {
            let (note, line_id) = row?;
            if let std::collections::hash_map::Entry::Vacant(entry) = source_lines.entry(note) {
                entry.insert(
                    crate::stock_notes::note_lines(conn, company, note)?
                        .into_iter()
                        .map(|line| (line.id, line))
                        .collect::<HashMap<_, _>>(),
                );
            }
            let line = source_lines[&note]
                .get(&line_id)
                .ok_or_else(crate::stock_notes::invalid_spans)?;
            if line.unbilled() <= 0
                || part
                    .location
                    .as_ref()
                    .is_some_and(|name| normalized(name) != normalized(&line.location))
                || part.batch.as_ref().is_some_and(|name| {
                    line.batch
                        .as_ref()
                        .is_none_or(|batch| normalized(name) != normalized(batch))
                })
            {
                continue;
            }
            let eligible = if let Some(due) = &part.order.expected_on {
                crate::stock_notes::unbilled_delivery_spans(conn, line, due)?
            } else {
                crate::stock_notes::allocate_spans(conn, line, line.unbilled())?
            };
            let free = unselected(
                eligible,
                planned
                    .get(&line_id)
                    .map_or(&[], |(_, spans)| spans.as_slice()),
            );
            if !free.is_empty() {
                total += free
                    .iter()
                    .map(|span| i128::from(span.end - span.start))
                    .sum::<i128>();
                candidates.push((note, line_id, free));
            }
        }
        if total < i128::from(part.quantity) {
            return Err(Error::Validation(format!(
                "Tracking number {} has insufficient eligible unbilled stock for {}",
                part.tracking, part.item
            )));
        }
        if candidates.len() > 1 && total != i128::from(part.quantity) {
            return Err(Error::Validation(format!(
                "Tracking number {} identifies multiple stock allocations for {}. Specify the source location, batch, order or due date.",
                part.tracking, part.item
            )));
        }
        let mut pending = part.quantity;
        for (note, line, free) in candidates {
            let (_, selected) = planned.entry(line).or_insert_with(|| (note, Vec::new()));
            for span in free {
                let quantity = (span.end - span.start).min(pending);
                if quantity > 0 {
                    selected.push(span.start..span.start + quantity);
                    pending -= quantity;
                }
            }
            selected.sort_by_key(|span| span.start);
        }
    }
    if conn.execute(
        "INSERT INTO imported_stock_returns(company_id, kind, party_ledger_id, date, reference, reference_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(company_id, kind, party_ledger_id, date, reference_key) DO NOTHING",
        params![company, source.kind.as_str(), party, source.date, source.reference, normalized(&source.reference)],
    )? != 1 {
        return Err(Error::Validation(format!("Stock return {} already exists", source.reference)));
    }
    let document = conn.last_insert_rowid();
    save_metadata(conn, company, "stockreturn", document, &source.metadata)?;
    let mut by_note = BTreeMap::<_, Vec<_>>::new();
    let mut preview = Vec::new();
    for (line, (note, spans)) in planned {
        preview.push(ReturnMatch {
            note_line: line,
            spans: spans.clone(),
        });
        by_note.entry(note).or_default().push(StockReturnSelection {
            note_line: line,
            spans,
        });
    }
    for (note, selections) in by_note {
        let id = write_stock_return(
            conn,
            company,
            note,
            &source.date,
            &source.reference,
            &source.reason,
            &selections,
        )?;
        conn.execute(
            "INSERT INTO imported_stock_return_parts(return_id, document_id) VALUES (?1, ?2)",
            params![id, document],
        )?;
    }
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(vouchers: &str) -> Vec<u8> {
        format!("<ENVELOPE><BODY><DATA><MESSAGE>{vouchers}</MESSAGE></DATA></BODY></ENVELOPE>")
            .into_bytes()
    }

    fn voucher(kind: &str, date: &str, reference: &str, fields: &str) -> String {
        format!(
            "<VOUCHER><DATE>{date}</DATE><VOUCHERTYPENAME>{kind}</VOUCHERTYPENAME>
             <VOUCHERNUMBER>{reference}</VOUCHERNUMBER><PARTYLEDGERNAME>Party</PARTYLEDGERNAME>{fields}</VOUCHER>"
        )
    }

    fn item(qty: i64, fields: &str) -> String {
        format!(
            "<INVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>{qty} pcs</ACTUALQTY>
             {fields}</INVENTORYENTRIES.LIST>"
        )
    }

    fn fixture(kind: InvoiceKind) -> (Store, i64, i64) {
        let store = Store::memory().unwrap();
        let company = store.create_company("Tracked returns").unwrap();
        let (party, trade) = match kind {
            InvoiceKind::Sale => (Category::Asset, Category::Income),
            InvoiceKind::Purchase => (Category::Liability, Category::Expense),
        };
        store.create_ledger(company, "Party", party).unwrap();
        store.create_ledger(company, "Trade", trade).unwrap();
        let part = store.create_stock_item(company, "Part", "pcs").unwrap();
        (store, company, part)
    }

    #[test]
    fn one_rejection_returns_from_two_deliveries_and_reverses_atomically() {
        let (mut store, company, part) = fixture(InvoiceKind::Sale);
        let location = store.stock_locations(company).unwrap()[0].id;
        store
            .adjust_stock_at(company, part, location, "2026-04-01", 8000, "Opening")
            .unwrap();
        let xml = voucher("Sales Order", "20260401", "ORDER", &item(4, r#"
            <RATE>10/pcs</RATE>
            <BATCHALLOCATIONS.LIST><ACTUALQTY>2 pcs</ACTUALQTY><ORDERDUEDATE>20260405</ORDERDUEDATE></BATCHALLOCATIONS.LIST>
            <BATCHALLOCATIONS.LIST><ACTUALQTY>2 pcs</ACTUALQTY><ORDERDUEDATE>20260412</ORDERDUEDATE></BATCHALLOCATIONS.LIST>
        "#))
            + &voucher("Delivery Note", "20260402", "EARLY", &item(-2,
                "<ORDERNO>ORDER</ORDERNO><ORDERDUEDATE>20260405</ORDERDUEDATE><TRACKINGNO>A</TRACKINGNO>"))
            + &voucher("Delivery Note", "20260403", "LATE", &item(-2,
                "<ORDERNO>ORDER</ORDERNO><ORDERDUEDATE>20260412</ORDERDUEDATE><TRACKINGNUMBER>B</TRACKINGNUMBER>"))
            + &voucher("Rejections In", "20260404", "RET", &item(2, r#"
                <BATCHALLOCATIONS.LIST><ACTUALQTY>1 pcs</ACTUALQTY><TRACKINGNO>A</TRACKINGNO></BATCHALLOCATIONS.LIST>
                <BATCHALLOCATIONS.LIST><ACTUALQTY>1 pcs</ACTUALQTY><TRACKINGNUMBER>B</TRACKINGNUMBER></BATCHALLOCATIONS.LIST>
            "#))
            + &voucher("Delivery Note", "20260404", "REPLACEMENT", &item(-1,
                "<ORDERNO>ORDER</ORDERNO><ORDERDUEDATE>20260412</ORDERDUEDATE>"));
        let plan = store.preview_xml_import(company, &envelope(&xml)).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        assert_eq!(plan.stock_return_count(), 1);
        assert!(store.stock_notes(company, None).unwrap().is_empty());
        assert_eq!(store.apply_xml_import(&plan).unwrap().stock_returns, 1);
        assert!(store.vouchers(company).unwrap().is_empty());
        let order = store.orders(company).unwrap()[0].id;
        let notes = store.stock_notes(company, Some(order)).unwrap();
        let find = |reference| {
            notes
                .iter()
                .find(|note| note.reference == reference)
                .unwrap()
                .id
        };
        let early = find("EARLY");
        let late = find("LATE");
        let progress = |store: &Store| {
            store
                .order_detail(company, order)
                .unwrap()
                .delivery_lots
                .iter()
                .map(|lot| lot.remaining)
                .collect::<Vec<_>>()
        };
        let stock = |store: &Store| {
            store
                .stock_register(company, part, None, None)
                .unwrap()
                .closing_quantity
        };
        assert_eq!(progress(&store), [1000, 0]);
        assert_eq!(stock(&store), 5000);
        let ret = store.stock_note_returns(company, early).unwrap().remove(0);
        assert_eq!(ret.source_notes, 2);
        assert!(store
            .reverse_stock_return(company, ret.id, "2026-04-05", "Cancel return")
            .is_err());
        assert_eq!(progress(&store), [1000, 0]);
        for note in [early, late] {
            assert!(store.stock_note_returns(company, note).unwrap()[0]
                .reversed_on
                .is_none());
        }
        store
            .reverse_stock_note(
                company,
                find("REPLACEMENT"),
                "2026-04-05",
                "Cancel replacement",
            )
            .unwrap();
        store
            .reverse_stock_return(company, ret.id, "2026-04-06", "Cancel return")
            .unwrap();
        assert_eq!(progress(&store), [0, 0]);
        assert_eq!(stock(&store), 4000);
        for note in [early, late] {
            assert_eq!(
                store.stock_note_returns(company, note).unwrap()[0]
                    .reversed_on
                    .as_deref(),
                Some("2026-04-06")
            );
        }
    }

    #[test]
    fn earlier_purchase_billing_protects_its_stock_in_a_combined_import() {
        let (mut store, company, part) = fixture(InvoiceKind::Purchase);
        let receipt = voucher(
            "Receipt Note",
            "20260401",
            "RECEIPT",
            &item(
                4,
                "<TRACKINGNUMBER>R1</TRACKINGNUMBER><RATE>8/pcs</RATE><AMOUNT>-32</AMOUNT>",
            ),
        );
        let bill = voucher(
            "Purchase",
            "20260402",
            "BILL",
            r#"
            <ISINVOICE>Yes</ISINVOICE>
            <LEDGERENTRIES.LIST><LEDGERNAME>Party</LEDGERNAME><AMOUNT>20</AMOUNT>
                <BILLALLOCATIONS.LIST><NAME>BILL</NAME><BILLTYPE>New Ref</BILLTYPE><AMOUNT>20</AMOUNT></BILLALLOCATIONS.LIST>
            </LEDGERENTRIES.LIST>
            <ALLINVENTORYENTRIES.LIST><STOCKITEMNAME>Part</STOCKITEMNAME><ACTUALQTY>2 pcs</ACTUALQTY><BILLEDQTY>2 pcs</BILLEDQTY>
                <RATE>10/pcs</RATE><AMOUNT>-20</AMOUNT><TRACKINGNUMBER>R1</TRACKINGNUMBER>
                <ACCOUNTINGALLOCATIONS.LIST><LEDGERNAME>Trade</LEDGERNAME><AMOUNT>-20</AMOUNT></ACCOUNTINGALLOCATIONS.LIST>
            </ALLINVENTORYENTRIES.LIST>
        "#,
        );
        let rejection = |qty| {
            voucher(
                "Rejections Out",
                "20260403",
                "RETURN",
                &item(qty, "<VOUCHERTRACKINGNUMBER>R1</VOUCHERTRACKINGNUMBER>"),
            )
        };
        let invalid = store
            .preview_xml_import(
                company,
                &envelope(&(receipt.clone() + &bill + &rejection(-3))),
            )
            .unwrap();
        assert!(!invalid.can_apply(), "{:?}", invalid.issues());
        assert!(
            invalid
                .issues()
                .iter()
                .any(|issue| issue.contains("eligible unbilled stock")),
            "{:?}",
            invalid.issues()
        );
        assert!(store.stock_notes(company, None).unwrap().is_empty());
        let tracked = store
            .preview_xml_import(company, &envelope(&(receipt + &bill + &rejection(-2))))
            .unwrap();
        assert!(tracked.can_apply(), "{:?}", tracked.issues());
        let result = store.apply_xml_import(&tracked).unwrap();
        assert_eq!((result.stock_returns, result.stock_matches), (1, 1));
        assert_eq!(store.vouchers(company).unwrap().len(), 1);
        assert_eq!(store.invoices(company).unwrap()[0].total, 2000);
        assert_eq!(
            store
                .stock_register(company, part, None, None)
                .unwrap()
                .closing_quantity,
            2000
        );
    }

    #[test]
    fn changed_return_positions_require_a_new_preview() {
        use crate::stock_returns::StockReturnLineDraft;
        let (mut store, company, part) = fixture(InvoiceKind::Purchase);
        store.create_stock_location(company, "Secondary").unwrap();
        let receipt = voucher(
            "Receipt Note",
            "20260401",
            "RECEIPT",
            &item(4, "<TRACKINGNUMBER>R1</TRACKINGNUMBER>"),
        ) + &voucher(
            "Receipt Note",
            "20260401",
            "SECONDARY",
            &item(
                2,
                "<TRACKINGNUMBER>R1</TRACKINGNUMBER><GODOWNNAME>Secondary</GODOWNNAME>",
            ),
        );
        let plan = store
            .preview_xml_import(company, &envelope(&receipt))
            .unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        let source = store
            .stock_notes(company, None)
            .unwrap()
            .into_iter()
            .find(|note| note.reference == "RECEIPT")
            .unwrap();
        let line = store.stock_note_lines(company, source.id).unwrap()[0].id;
        let imported = voucher(
            "Rejections Out",
            "20260403",
            "IMPORT",
            &item(-1, "<TRACKINGNUMBER>R1</TRACKINGNUMBER>"),
        );
        let ambiguous = store
            .preview_xml_import(company, &envelope(&imported))
            .unwrap();
        assert!(
            !ambiguous.can_apply()
                && ambiguous
                    .issues()
                    .iter()
                    .any(|issue| issue.contains("multiple stock allocations"))
        );
        let imported = imported.replace(
            "<TRACKINGNUMBER>R1</TRACKINGNUMBER>",
            "<TRACKINGNUMBER>R1</TRACKINGNUMBER><GODOWNNAME>Main</GODOWNNAME>",
        );
        let preview = store
            .preview_xml_import(company, &envelope(&imported))
            .unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        store
            .post_stock_return(
                company,
                source.id,
                "2026-04-02",
                "MANUAL",
                "Return before import",
                &[StockReturnLineDraft {
                    note_line: line,
                    quantity: 1000,
                }],
            )
            .unwrap();
        let error = store.apply_xml_import(&preview).unwrap_err();
        assert!(error.to_string().contains("return allocations changed"));
        assert_eq!(
            store.stock_note_returns(company, source.id).unwrap().len(),
            1
        );
        assert_eq!(
            store
                .stock_register(company, part, None, None)
                .unwrap()
                .closing_quantity,
            5000
        );
    }
}
