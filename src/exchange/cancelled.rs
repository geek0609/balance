//! Voucher status and cancelled records in XML imports.
use super::*;

pub(super) const KIND: &str = "cancelledvoucher";
const POSTED_KINDS: &[&str] = &[
    "voucher",
    "stocknote",
    "order",
    "stockreturn",
    "stockcount",
    "stockjournal",
];

pub(super) fn is_posted_kind(kind: &str) -> bool {
    POSTED_KINDS.contains(&kind)
}

pub(super) fn other_status_seen(
    kind: &str,
    key: &str,
    value: &str,
    seen: &HashSet<(String, String, String)>,
) -> bool {
    if key == "master" {
        return false;
    }
    let seen_kind =
        |kind: &str| seen.contains(&(kind.to_owned(), key.to_owned(), value.to_owned()));
    if kind == KIND {
        POSTED_KINDS.iter().any(|kind| seen_kind(kind))
    } else {
        is_posted_kind(kind) && seen_kind(KIND)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Posted,
    Cancelled,
    Optional,
}

pub(super) fn status_field(field: &str) -> bool {
    field.eq_ignore_ascii_case("ISCANCELLED") || field.eq_ignore_ascii_case("ISOPTIONAL")
}

pub(super) fn status(node: Node<'_, '_>) -> Result<Status> {
    let flag = |name| -> Result<bool> {
        match read_scalar(node, name, Some(name), |value| Ok(normalized(value)))?.as_deref() {
            None | Some("no") => Ok(false),
            Some("yes") => Ok(true),
            _ => Err(Error::Validation(format!("Invalid {name} flag"))),
        }
    };
    let cancelled = flag("ISCANCELLED")?;
    let optional = flag("ISOPTIONAL")?;
    Ok(match (cancelled, optional) {
        (true, _) => Status::Cancelled,
        (false, true) => Status::Optional,
        _ => Status::Posted,
    })
}

fn read_scalar<T: PartialEq>(
    node: Node<'_, '_>,
    field: &str,
    attribute: Option<&str>,
    parse: impl Fn(&str) -> Result<T>,
) -> Result<Option<T>> {
    let mut fields = node
        .children()
        .filter(|child| child.is_element() && local_name(*child).eq_ignore_ascii_case(field));
    let child = fields.next();
    if fields.next().is_some() {
        return Err(Error::Validation(format!("{field} occurs more than once")));
    }
    if child.is_some_and(|child| child.children().any(|content| content.is_element())) {
        return Err(Error::Validation(format!(
            "Nested {field} is not supported"
        )));
    }
    let child = direct_text(node, field)
        .filter(|text| !text.is_empty())
        .map(|text| parse(&text))
        .transpose()?;
    let mut attributes = node
        .attributes()
        .filter(|attr| attribute.is_some_and(|name| attr.name().eq_ignore_ascii_case(name)));
    let value = attributes
        .next()
        .map(|attr| attr.value().trim())
        .filter(|text| !text.is_empty())
        .map(&parse)
        .transpose()?;
    if attributes.next().is_some() {
        return Err(Error::Validation(format!(
            "The {field} attribute occurs more than once"
        )));
    }
    if matches!((&child, &value), (Some(child), Some(value)) if child != value) {
        return Err(Error::Validation(format!("Conflicting {field} values")));
    }
    Ok(child.or(value))
}

#[derive(Debug, Clone)]
pub(super) struct ImportedCancelled {
    date: String,
    voucher_type: String,
    reference: String,
    party: String,
    narration: String,
    pub metadata: SourceMetadata,
}

type HeaderKey = (String, String, String, String, String);

impl ImportedCancelled {
    pub(super) fn header_without_id(&self) -> Option<HeaderKey> {
        (!self.has_source_id()).then(|| {
            (
                self.date.clone(),
                normalized(&self.voucher_type),
                normalized(&self.reference),
                self.party.clone(),
                self.narration.clone(),
            )
        })
    }

    fn has_source_id(&self) -> bool {
        self.metadata
            .identities()
            .iter()
            .any(|(_, value)| !value.is_empty())
    }
}

pub(super) fn existing_headers(conn: &Connection, company: i64) -> Result<HashSet<HeaderKey>> {
    let mut stmt = conn.prepare(
        "SELECT date, type_key, reference_key, party, narration FROM cancelled_vouchers
         WHERE company_id = ?1 AND has_source_id = 0",
    )?;
    let rows = stmt.query_map([company], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(super) fn parse(node: Node<'_, '_>, metadata: SourceMetadata) -> Result<ImportedCancelled> {
    for field in ["GUID", "REMOTEID", "MASTERID", "ALTERID", "VCHKEY"] {
        read_scalar(node, field, Some(field), |value| Ok(value.to_owned()))?;
    }
    let action = read_scalar(node, "ACTION", Some("ACTION"), |value| {
        Ok(normalized(value))
    })?;
    if action.is_some_and(|action| action != "create") {
        return Err(Error::Validation("Only new records can be imported".into()));
    }
    let date = read_scalar(node, "DATE", Some("DATE"), xml_date)?
        .ok_or_else(|| Error::Validation("Missing cancellation voucher date".into()))?;
    let type_key = read_scalar(node, "VOUCHERTYPENAME", Some("VCHTYPE"), |value| {
        Ok(normalized(value))
    })?
    .ok_or_else(|| Error::Validation("Missing cancellation voucher type".into()))?;
    let voucher_type = direct_text(node, "VOUCHERTYPENAME")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            node.attributes()
                .find(|attr| attr.name().eq_ignore_ascii_case("VCHTYPE"))
                .map(|attr| attr.value().trim().to_owned())
        })
        .unwrap_or(type_key);
    let text = |field| {
        read_scalar(node, field, None, |value| Ok(value.to_owned())).map(Option::unwrap_or_default)
    };
    Ok(ImportedCancelled {
        date,
        voucher_type,
        reference: text("VOUCHERNUMBER")?,
        party: text("PARTYLEDGERNAME")?,
        narration: text("NARRATION")?,
        metadata,
    })
}

pub(super) fn write(conn: &Connection, company: i64, record: &ImportedCancelled) -> Result<()> {
    let inserted = conn.execute(
        "INSERT INTO cancelled_vouchers(company_id, date, voucher_type, reference, party, narration, type_key, reference_key, has_source_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT DO NOTHING",
        params![
            company, record.date, record.voucher_type, record.reference, record.party, record.narration,
            normalized(&record.voucher_type), normalized(&record.reference), record.has_source_id(),
        ],
    )?;
    if inserted == 0 {
        return Err(Error::Validation(format!(
            "Cancelled voucher {} dated {} already exists",
            record.reference, record.date
        )));
    }
    save_metadata(
        conn,
        company,
        KIND,
        conn.last_insert_rowid(),
        &record.metadata,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(contents: &str) -> String {
        format!("<ENVELOPE><BODY><DATA><MESSAGE>{contents}</MESSAGE></DATA></BODY></ENVELOPE>")
    }

    fn company() -> (Store, i64) {
        let store = Store::memory().unwrap();
        let company = store.create_company("History").unwrap();
        store
            .create_ledger(company, "Cash", Category::Asset)
            .unwrap();
        store
            .create_ledger(company, "Sales", Category::Income)
            .unwrap();
        (store, company)
    }

    fn journal(guid: &str) -> String {
        format!(
            r#"<VOUCHER><DATE>20260402</DATE><VOUCHERTYPENAME>Journal</VOUCHERTYPENAME>
               <VOUCHERNUMBER>J-1</VOUCHERNUMBER><GUID>{guid}</GUID>
               <ISCANCELLED TYPE="Logical">No</ISCANCELLED><ISOPTIONAL>No</ISOPTIONAL>
               <LEDGERENTRIES.LIST><LEDGERNAME>Cash</LEDGERNAME><AMOUNT>-25</AMOUNT></LEDGERENTRIES.LIST>
               <LEDGERENTRIES.LIST><LEDGERNAME>Sales</LEDGERNAME><AMOUNT>25</AMOUNT></LEDGERENTRIES.LIST>
             </VOUCHER>"#
        )
    }

    fn archived(guid: &str) -> String {
        format!(
            r#"<VOUCHER DATE="20260402" VCHTYPE="Journal">
              <ISCANCELLED>Yes</ISCANCELLED><GUID>{guid}</GUID><VOUCHERNUMBER>J-1</VOUCHERNUMBER>
              <PARTYLEDGERNAME>Example Customer</PARTYLEDGERNAME><NARRATION>Duplicate entry</NARRATION>
             </VOUCHER>"#
        )
    }

    #[test]
    fn imports_cancelled_history_alongside_reused_numbers_without_posting_it() {
        let (mut store, company) = company();
        store
            .set_period_lock(company, Some("2026-04-01"), "Prior period closed")
            .unwrap();
        let old = format!(
            r#"<VOUCHER DATE="20260401" VCHTYPE="Warehouse Move" ISCANCELLED="Yes">
            <DATE>2026-04-01</DATE><VOUCHERNUMBER>OLD-1</VOUCHERNUMBER><VCHKEY>history-key</VCHKEY>
            <NARRATION>Warehouse movement cancelled before posting</NARRATION>
            <INVENTORYENTRIESOUT.LIST><STOCKITEMNAME>Unavailable product</STOCKITEMNAME></INVENTORYENTRIESOUT.LIST>
            <STOCKITEM NAME="Unposted item" />
            {}
          </VOUCHER>"#,
            journal("unposted")
        );
        let xml = envelope(&(journal("replacement") + &archived("original") + &old));
        let preview = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(preview.can_apply(), "{:?}", preview.issues());
        assert_eq!(
            (preview.voucher_count(), preview.cancelled_voucher_count()),
            (1, 2)
        );
        assert!(store
            .cancelled_vouchers(company, None, None, None, "")
            .unwrap()
            .is_empty());
        let counts = store.apply_xml_import(&preview).unwrap();
        assert_eq!((counts.vouchers, counts.cancelled_vouchers), (1, 2));
        assert!(store.stock_items(company).unwrap().is_empty());
        assert!(store.stock_journals(company).unwrap().is_empty());
        let posted = store.day_book(company, None, None, None, "J-1").unwrap();
        assert_eq!(posted.len(), 1);
        assert_eq!(
            (posted[0].debit_total, posted[0].credit_total),
            (2500, 2500)
        );
        let cancelled = store
            .cancelled_vouchers(
                company,
                Some("2026-04-02"),
                Some("2026-04-02"),
                Some(VoucherKind::Journal),
                "customer",
            )
            .unwrap();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].reference, "J-1");
        assert_eq!(cancelled[0].narration, "Duplicate entry");
        let key: String = store.conn.query_row(
            "SELECT voucher_key FROM imported_record_metadata WHERE company_id = ?1 AND kind = ?2 AND voucher_key != ''",
            params![company, KIND],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(key, "history-key");
        assert_eq!(
            store
                .cancelled_vouchers(company, None, None, None, "warehouse move")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn rejects_conflicting_posting_statuses_during_preview_and_transaction() {
        for cancelled_first in [true, false] {
            let (mut store, company) = company();
            let posted = envelope(&journal("shared-guid"));
            let cancelled = envelope(&archived("SHARED-GUID"));
            let plans = [posted.as_bytes(), cancelled.as_bytes()].map(|xml| {
                let plan = store.preview_xml_import(company, xml).unwrap();
                assert!(plan.can_apply(), "{:?}", plan.issues());
                plan
            });
            let combined = envelope(&(journal("shared-guid") + &archived("SHARED-GUID")));
            let combined = store
                .preview_xml_import(company, combined.as_bytes())
                .unwrap();
            assert!(combined
                .issues()
                .iter()
                .any(|issue| issue.contains("different posting status")));
            let first = usize::from(cancelled_first);
            store.apply_xml_import(&plans[first]).unwrap();
            let stale = store
                .apply_xml_import(&plans[1 - first])
                .unwrap_err()
                .to_string();
            assert!(stale.contains("different posting status"), "{stale}");
            let next = if cancelled_first { posted } else { cancelled };
            let preview = store.preview_xml_import(company, next.as_bytes()).unwrap();
            assert!(preview
                .issues()
                .iter()
                .any(|issue| issue.contains("different posting status")));
            assert_eq!(store.vouchers(company).unwrap().len(), 1 - first);
            assert_eq!(
                store
                    .cancelled_vouchers(company, None, None, None, "")
                    .unwrap()
                    .len(),
                first
            );
        }
    }

    #[test]
    fn prevents_replays_without_identifiers_and_validates_status_flags() {
        let (mut store, company) = company();
        let xml = envelope(&archived(""));
        let plan = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(plan.can_apply(), "{:?}", plan.issues());
        store.apply_xml_import(&plan).unwrap();
        assert!(store.apply_xml_import(&plan).is_err());
        let replay = store.preview_xml_import(company, xml.as_bytes()).unwrap();
        assert!(replay
            .issues()
            .iter()
            .any(|issue| issue.contains("cancelled voucher already exists")));
        for (invalid, expected) in [
            (
                xml.replace("Yes</ISCANCELLED>", "Unknown</ISCANCELLED>"),
                "Invalid ISCANCELLED",
            ),
            (
                xml.replace(
                    "Yes</ISCANCELLED>",
                    "Yes</ISCANCELLED><ISCANCELLED>No</ISCANCELLED>",
                ),
                "occurs more than once",
            ),
            (
                xml.replace(
                    "Yes</ISCANCELLED>",
                    "No</ISCANCELLED><ISOPTIONAL>Yes</ISOPTIONAL>",
                ),
                "optional vouchers require review",
            ),
        ] {
            let preview = store
                .preview_xml_import(company, invalid.as_bytes())
                .unwrap();
            assert!(
                preview
                    .issues()
                    .iter()
                    .any(|issue| issue.contains(expected)),
                "{:?}",
                preview.issues()
            );
        }
        assert!(store.vouchers(company).unwrap().is_empty());
        assert_eq!(
            store
                .cancelled_vouchers(company, None, None, None, "")
                .unwrap()
                .len(),
            1
        );
    }
}
