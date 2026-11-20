//! Printable account statements, trade documents and payroll records.
use crate::{
    format_amount,
    inventory::{format_quantity, line_amount, InvoiceDiscount, InvoiceKind},
    parse_date,
    quotations::quotation,
    stock_notes::{get_note, note_label},
    Error, Result, Store,
};
use rusqlite::{params, OptionalExtension};
use std::{fmt::Write as _, io::Write as _, path::Path};

fn escape(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
    output
}

fn money(value: i128) -> String {
    let magnitude = value.unsigned_abs();
    format!(
        "{}{}.{:02}",
        if value < 0 { "-" } else { "" },
        magnitude / 100,
        magnitude % 100
    )
}

fn statement_balance(value: i128) -> String {
    let amount = money(value);
    match value.cmp(&0) {
        std::cmp::Ordering::Less => format!("{} Cr", amount.trim_start_matches('-')),
        std::cmp::Ordering::Equal => amount,
        std::cmp::Ordering::Greater => format!("{amount} Dr"),
    }
}

fn printable_html(title: &str) -> String {
    ["<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>", &escape(title), "</title><style>body{font:14px system-ui,sans-serif;color:#182431;max-width:900px;margin:2rem auto;padding:0 1rem}h1{margin-bottom:.2rem}p{white-space:pre-line}table{border-collapse:collapse;width:100%;margin:1.5rem 0}th,td{text-align:left;border-bottom:1px solid #ccd3db;padding:.55rem;vertical-align:top}.num{text-align:right;white-space:nowrap}.totals{margin-left:auto;width:min(100%,360px)}.totals td{border:0}.total{font-weight:bold;border-top:2px solid #182431}.note{color:#4c5b68;font-size:12px}@media print{body{margin:0;max-width:none}}</style></head><body>"].concat()
}

fn html_destination(destination: &Path) -> Result<&Path> {
    if !destination.is_absolute()
        || !destination
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case("html"))
    {
        return Err(Error::Validation(
            "Enter an absolute path to a new .html file".into(),
        ));
    }
    Ok(destination)
}

fn write_html(destination: &Path, html: &str) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| Error::Validation(format!("Cannot create document file: {error}")))?;
    file.write_all(html.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|error| Error::Validation(format!("Cannot finish document file: {error}")))?;
    Ok(())
}

#[cfg(test)]
mod statement_tests {
    use crate::{Category, Posting, Store, VoucherKind};

    #[test]
    fn ledger_statement_prints_escaped_references_and_dated_balances() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Demo & Co").unwrap();
        let ledger = store
            .create_ledger(company, "Test account", Category::Asset)
            .unwrap();
        let offset = store
            .create_ledger(company, "Offset", Category::Income)
            .unwrap();
        for (date, reference, amount) in [
            ("2026-01-01", "OPENING", -2500),
            ("2026-01-02", "DEMO <2>", 3000),
            ("2026-01-03", "AFTER-CUTOFF", 1000),
        ] {
            store
                .post_voucher_with_reference(
                    company,
                    VoucherKind::Journal,
                    reference,
                    date,
                    "Demo <entry>",
                    &[
                        Posting {
                            ledger_id: ledger,
                            amount,
                        },
                        Posting {
                            ledger_id: offset,
                            amount: -amount,
                        },
                    ],
                )
                .unwrap();
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "balance-statement-{}-{nonce}.html",
            std::process::id()
        ));
        let start = Some("2026-01-02");
        let end = Some("2026-01-02");
        store
            .export_ledger_statement_html(company, ledger, start, end, &path)
            .unwrap();
        let rejected_overwrite = store
            .export_ledger_statement_html(company, ledger, start, end, &path)
            .is_err();
        let html = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(rejected_overwrite);
        assert!(html.contains("Demo &amp; Co"));
        assert!(html.contains("DEMO &lt;2&gt;"));
        assert!(html.contains("Demo &lt;entry&gt;"));
        assert!(!html.contains("OPENING"));
        assert!(!html.contains("AFTER-CUTOFF"));
        assert!(html.contains("25.00 Cr"));
        assert!(html.contains("5.00 Dr"));
    }
}

impl Store {
    pub fn export_ledger_statement_html(
        &self,
        company_id: i64,
        ledger_id: i64,
        start: Option<&str>,
        end: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = html_destination(destination.as_ref())?;
        let tx = self.conn.unchecked_transaction()?;
        let register = self.ledger_register(company_id, ledger_id, start, end)?;
        let company = self
            .companies()?
            .into_iter()
            .find(|c| c.id == company_id)
            .ok_or_else(|| Error::Validation("Company not found".into()))?;
        let party = self.ledger_profile(company_id, ledger_id)?;
        let business = self.company_profile(company_id)?;
        let currency = self.company_base_currency(company_id)?;
        tx.commit()?;

        let mut html = printable_html("Statement of account");
        let _ = write!(
            html,
            "<h1>Statement of account</h1><p><strong>{}</strong>",
            escape(&company.name)
        );
        for value in [
            &business.address,
            &business.region,
            &business.postal_code,
            &business.country,
        ] {
            if !value.trim().is_empty() {
                let _ = write!(html, "<br>{}", escape(value));
            }
        }
        let _ = write!(
            html,
            "</p><p>Account <strong>{}</strong>",
            escape(&register.ledger.name)
        );
        for value in [
            &party.mailing_name,
            &party.address,
            &party.region,
            &party.postal_code,
            &party.country,
        ] {
            if !value.trim().is_empty() && value.trim() != register.ledger.name {
                let _ = write!(html, "<br>{}", escape(value));
            }
        }
        let from = start
            .map(parse_date)
            .transpose()?
            .map(|date| date.to_string());
        let through = end
            .map(parse_date)
            .transpose()?
            .map(|date| date.to_string());
        let _ = write!(
            html,
            "</p><p>{} through {}",
            from.as_deref().unwrap_or("First entry"),
            through.as_deref().unwrap_or("last saved date")
        );
        if let Some(code) = currency {
            let _ = write!(html, "<br>Currency {}", escape(&code));
        }
        html.push_str("</p><table><thead><tr><th>Date</th><th>Reference</th><th>Narration</th><th class=\"num\">Debit</th><th class=\"num\">Credit</th><th class=\"num\">Balance</th></tr></thead><tbody>");
        let _ = write!(
            html,
            "<tr><th colspan=\"5\">Opening balance</th><td class=\"num\">{}</td></tr>",
            statement_balance(register.opening_balance)
        );
        for entry in register.entries {
            let amount = i128::from(entry.amount);
            let debit = if amount > 0 {
                money(amount)
            } else {
                String::new()
            };
            let credit = if amount < 0 {
                money(-amount)
            } else {
                String::new()
            };
            let reference = if entry.reference.is_empty() {
                format!("Voucher {}", entry.voucher_id)
            } else {
                escape(&entry.reference)
            };
            let _ = write!(html, "<tr><td>{}</td><td>{reference}</td><td>{}</td><td class=\"num\">{debit}</td><td class=\"num\">{credit}</td><td class=\"num\">{}</td></tr>", escape(&entry.date), escape(&entry.narration), statement_balance(entry.running_balance));
        }
        let _ = write!(html, "</tbody><tfoot><tr class=\"total\"><th colspan=\"5\">Closing balance</th><td class=\"num\">{}</td></tr></tfoot></table></body></html>", statement_balance(register.closing_balance));
        write_html(destination, &html)
    }

    pub fn export_invoice_credit_html(
        &self,
        company: i64,
        note: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = html_destination(destination.as_ref())?;
        let tx = self.conn.unchecked_transaction()?;
        let (business, party, kind, invoice, invoice_date, voucher, date, reference, amount, reason, reversed_on, reversal_reference, reversal_reason, refundable) = tx.query_row(
            "SELECT c.name, p.name, i.kind, iv.reference, iv.date, nv.id, nv.date, nv.reference,
                    n.amount, n.reason, rv.date, rv.reference, n.reversal_reason, COALESCE(cash.amount, 0)
             FROM invoice_credit_notes n
             JOIN companies c ON c.id = n.company_id
             JOIN invoices i ON i.id = n.invoice_id AND i.company_id = n.company_id
             JOIN ledgers p ON p.id = i.party_ledger_id AND p.company_id = n.company_id
             JOIN vouchers iv ON iv.id = i.voucher_id AND iv.company_id = n.company_id
             JOIN vouchers nv ON nv.id = n.voucher_id AND nv.company_id = n.company_id
             LEFT JOIN vouchers rv ON rv.id = n.reversal_voucher_id AND rv.company_id = n.company_id
             LEFT JOIN invoice_refund_credits cash ON cash.note_id = n.id
             WHERE n.company_id = ?1 AND n.id = ?2",
            params![company, note],
            |r| Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?, r.get::<_, String>(7)?, r.get::<_, i64>(8)?,
                r.get::<_, String>(9)?, r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?, r.get::<_, Option<String>>(12)?, r.get::<_, i64>(13)?,
            )),
        ).optional()?.ok_or_else(|| Error::Validation("Invoice note not found in the selected company".into()))?;
        let label = match InvoiceKind::parse(&kind)? {
            InvoiceKind::Sale => "Credit note",
            InvoiceKind::Purchase => "Debit note",
        };
        let title = format!("{label} {reference}");
        let mut html = printable_html(&title);
        let _ = write!(
            html,
            "<h1>{}</h1><p><strong>{}</strong><br>Party: {}<br>Date: {}<br>Invoice: {} ({})<br>Voucher: {voucher}</p><p>{}</p>",
            escape(&title), escape(&business), escape(&party), escape(&date),
            escape(&invoice), escape(&invoice_date), escape(&reason),
        );
        if let Some(date) = reversed_on {
            let _ = write!(
                html,
                "<p><strong>Reversed {}</strong><br>Reference: {}<br>{}</p>",
                escape(&date),
                escape(reversal_reference.as_deref().unwrap_or_default()),
                escape(reversal_reason.as_deref().unwrap_or_default())
            );
        }
        if refundable > 0 {
            let _ = write!(
                html,
                "<p>Applied to invoice {}<br>Refundable credit issued {}</p>",
                format_amount(amount - refundable),
                format_amount(refundable)
            );
        }
        for (reference, amount) in crate::bill_splits::note_allocations(&tx, note)? {
            let _ = write!(
                html,
                "<p>Applied to bill {} · {}</p>",
                escape(&reference),
                format_amount(amount)
            );
        }
        let returned = self.invoice_return_lines(company, note)?;
        let document_label = if returned.is_empty() {
            "Financial adjustment record"
        } else {
            "Stock and accounting return record"
        };
        if !returned.is_empty() {
            html.push_str("<table><thead><tr><th>Item</th><th>Location</th><th>Batch</th><th class=\"num\">Returned</th><th class=\"num\">Net item credit</th></tr></thead><tbody>");
            for line in returned {
                let _ = write!(html, "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{} {}</td><td class=\"num\">{}</td></tr>",
                    escape(&line.item), escape(&line.location), escape(line.batch.as_deref().unwrap_or("Unbatched")),
                    format_quantity(line.quantity.into()), escape(&line.unit), format_amount(line.net_amount));
            }
            html.push_str("</tbody></table>");
        }
        html.push_str("<table><thead><tr><th>Ledger</th><th>Component</th><th class=\"num\">Reduction</th></tr></thead><tbody>");
        {
            let mut stmt = tx.prepare(
                "SELECT l.name, d.is_tax, d.reduction FROM invoice_credit_lines d
                 JOIN ledgers l ON l.id = d.ledger_id AND l.company_id = ?1
                 WHERE d.note_id = ?2 ORDER BY d.is_tax, l.name, l.id",
            )?;
            let mut rows = stmt.query(params![company, note])?;
            while let Some(row) = rows.next()? {
                let ledger: String = row.get(0)?;
                let tax: bool = row.get(1)?;
                let reduction: i64 = row.get(2)?;
                let component = if tax {
                    "Tax"
                } else if reduction < 0 {
                    "Deduction"
                } else {
                    "Trade"
                };
                let _ = write!(
                    html,
                    "<tr><td>{}</td><td>{component}</td><td class=\"num\">{}</td></tr>",
                    escape(&ledger),
                    format_amount(reduction)
                );
            }
        }
        tx.commit()?;
        let _ = write!(html, "</tbody><tfoot><tr class=\"total\"><td colspan=\"2\">Net invoice reduction</td><td class=\"num\">{}</td></tr></tfoot></table><p class=\"note\">{document_label}</p></body></html>", format_amount(amount));
        write_html(destination, &html)
    }

    pub fn export_stock_note_html(
        &self,
        company_id: i64,
        note_id: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = html_destination(destination.as_ref())?;
        let company = self
            .companies()?
            .into_iter()
            .find(|company| company.id == company_id)
            .ok_or_else(|| Error::Validation("Company not found".into()))?;
        let note = get_note(&self.conn, company_id, note_id)?;
        let lines = self.stock_note_lines(company_id, note_id)?;
        let title = format!("{} {}", note_label(note.kind), note.reference);
        let mut html = printable_html(&title);
        let _ = write!(
            html,
            "<h1>{}</h1><p><strong>{}</strong><br>Party: {}<br>Date: {}",
            escape(&title),
            escape(&company.name),
            escape(&note.party),
            escape(&note.date),
        );
        if let Some(date) = &note.reversed_on {
            let _ = write!(html, "<br><strong>Reversed {}</strong>", escape(date));
        }
        html.push_str("</p><table><thead><tr><th>Item</th><th>Location</th><th>Batch</th><th class=\"num\">Quantity</th></tr></thead><tbody>");
        for line in lines {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{} {}</td></tr>",
                escape(&line.item),
                escape(&line.location),
                escape(line.batch.as_deref().unwrap_or("Unbatched")),
                format_quantity(i128::from(line.values.quantity)),
                escape(&line.unit),
            );
        }
        html.push_str("</tbody></table>");
        if !note.narration.is_empty() {
            let _ = write!(html, "<p>{}</p>", escape(&note.narration));
        }
        html.push_str("</body></html>");
        write_html(destination, &html)
    }

    pub fn export_stock_return_html(
        &self,
        company_id: i64,
        note_id: i64,
        return_id: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = html_destination(destination.as_ref())?;
        let tx = self.conn.unchecked_transaction()?;
        let company = self
            .companies()?
            .into_iter()
            .find(|company| company.id == company_id)
            .ok_or_else(|| Error::Validation("Company not found".into()))?;
        let note = get_note(&tx, company_id, note_id)?;
        let returned = self
            .stock_note_returns(company_id, note_id)?
            .into_iter()
            .find(|stock_return| stock_return.id == return_id)
            .ok_or_else(|| Error::Validation("Return not found for this stock note".into()))?;
        tx.commit()?;
        let label = match note.kind {
            InvoiceKind::Sale => "Customer stock return",
            InvoiceKind::Purchase => "Supplier stock return",
        };
        let title = format!("{label} {}", returned.reference);
        let mut html = printable_html(&title);
        let _ = write!(html, "<h1>{}</h1><p><strong>{}</strong><br>Party {}<br>Date {}<br>Against {} {} ({})</p><p>{}</p>",
            escape(&title), escape(&company.name), escape(&note.party), escape(&returned.date),
            note_label(note.kind), escape(&note.reference), escape(&note.date), escape(&returned.reason));
        if let (Some(date), Some(reason)) = (&returned.reversed_on, &returned.reversal_reason) {
            let _ = write!(
                html,
                "<p><strong>Reversed {}</strong><br>{}</p>",
                escape(date),
                escape(reason)
            );
        }
        html.push_str("<table><thead><tr><th>Item</th><th>Location</th><th>Batch</th><th class=\"num\">Returned quantity</th></tr></thead><tbody>");
        for line in returned.lines {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{} {}</td></tr>",
                escape(&line.item),
                escape(&line.location),
                escape(line.batch.as_deref().unwrap_or("Unbatched")),
                format_quantity(line.quantity.into()),
                escape(&line.unit)
            );
        }
        html.push_str("</tbody></table></body></html>");
        write_html(destination, &html)
    }

    pub fn export_order_html(
        &self,
        company_id: i64,
        order_id: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = html_destination(destination.as_ref())?;
        let company = self
            .companies()?
            .into_iter()
            .find(|c| c.id == company_id)
            .ok_or_else(|| Error::Validation("Company not found".into()))?;
        let detail = self.order_detail(company_id, order_id)?;
        let order = &detail.summary;
        let label = match order.kind {
            InvoiceKind::Sale => "Sales order",
            InvoiceKind::Purchase => "Purchase order",
        };
        let title = format!("{label} {}", order.reference);
        let mut html = printable_html(&title);
        let _ = write!(
            html,
            "<h1>{}</h1><p><strong>{}</strong><br>Party: {}<br>Date: {}<br>Status: {}",
            escape(&title),
            escape(&company.name),
            escape(&order.party),
            escape(&order.date),
            order.status.label(),
        );
        if let Some(date) = &order.expected_on {
            let _ = write!(html, "<br>Expected delivery: {}", escape(date));
        }
        if let Some(currency) = self.company_base_currency(company_id)? {
            let _ = write!(html, "<br>Currency: {}", escape(&currency));
        }
        if let Some(reference) = self.order_quotation(company_id, order_id)? {
            let _ = write!(html, "<br>Quotation: {}", escape(&reference));
        }
        html.push_str("</p><table><thead><tr><th>Item</th><th class=\"num\">Ordered</th><th class=\"num\">Fulfilled</th><th class=\"num\">Invoiced</th><th class=\"num\">Pre-closed</th><th class=\"num\">Unit price</th><th class=\"num\">Amount</th></tr></thead><tbody>");
        for row in detail.lines {
            let line = row.line;
            let amount = line_amount(line.quantity, line.unit_price)?;
            let _ = write!(
                html,
                "<tr><td>{}</td><td class=\"num\">{} {}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                escape(&row.item),
                format_quantity(line.quantity.into()),
                escape(&row.unit),
                format_quantity(row.fulfilled.into()),
                format_quantity(row.billed.into()),
                format_quantity(row.preclosed.into()),
                format_amount(line.unit_price),
                format_amount(amount),
            );
        }
        let _ = write!(html, "</tbody></table><table class=\"totals\"><tr class=\"total\"><td>Order subtotal</td><td class=\"num\">{}</td></tr></table>", money(detail.total));
        if !order.notes.is_empty() {
            let _ = write!(
                html,
                "<p><strong>Notes</strong><br>{}</p>",
                escape(&order.notes)
            );
        }
        if let (Some(date), Some(reason)) = (&order.closed_at, &order.close_reason) {
            let _ = write!(
                html,
                "<p><strong>Closed {} UTC</strong><br>{}</p>",
                escape(date),
                escape(reason)
            );
        }
        html.push_str("</body></html>");
        write_html(destination, &html)
    }

    pub fn export_quotation_html(
        &self,
        company_id: i64,
        quote_id: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = html_destination(destination.as_ref())?;
        let company = self
            .companies()?
            .into_iter()
            .find(|company| company.id == company_id)
            .ok_or_else(|| Error::Validation("Company not found".into()))?;
        let quote = quotation(&self.conn, company_id, quote_id)?;
        let lines = self.quotation_lines(company_id, quote_id)?;
        let items = self.stock_items(company_id)?;
        let title = format!("{} quotation {}", quote.kind.as_str(), quote.reference);
        let mut html = printable_html(&title);
        let _ = write!(
            html,
            "<h1>{}</h1><p><strong>{}</strong><br>Party: {}<br>Date: {}<br>Status: {}",
            escape(&title),
            escape(&company.name),
            escape(&quote.party),
            escape(&quote.date),
            quote.status(),
        );
        if let Some(reference) = &quote.revises {
            let _ = write!(html, "<br>Revises quotation {}", escape(reference));
        }
        if let Some(reference) = &quote.revised_by {
            let _ = write!(html, "<br>Replaced by quotation {}", escape(reference));
        }
        if let Some(date) = &quote.valid_through {
            let _ = write!(html, "<br>Valid through: {}", escape(date));
        }
        if let Some(date) = &quote.expected_on {
            let _ = write!(html, "<br>Expected delivery: {}", escape(date));
        }
        if let Some(currency) = self.company_base_currency(company_id)? {
            let _ = write!(html, "<br>Currency: {}", escape(&currency));
        }
        html.push_str("</p><table><thead><tr><th>Item</th><th class=\"num\">Quantity</th><th class=\"num\">Unit price</th><th class=\"num\">Amount</th></tr></thead><tbody>");
        let mut subtotal = 0_i128;
        for line in lines {
            let item = items
                .iter()
                .find(|item| item.id == line.item_id)
                .ok_or_else(|| Error::Validation("A quoted item is unavailable".into()))?;
            let amount = line_amount(line.quantity, line.unit_price)?;
            subtotal += i128::from(amount);
            let _ = write!(
                html,
                "<tr><td>{}</td><td class=\"num\">{} {}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                escape(&item.name),
                format_quantity(i128::from(line.quantity)),
                escape(&item.unit),
                format_amount(line.unit_price),
                format_amount(amount),
            );
        }
        let _ = write!(
            html,
            "</tbody></table><table class=\"totals\"><tr class=\"total\"><td>Quoted subtotal</td><td class=\"num\">{}</td></tr></table>",
            money(subtotal),
        );
        if !quote.notes.is_empty() {
            let _ = write!(
                html,
                "<p><strong>Notes</strong><br>{}</p>",
                escape(&quote.notes)
            );
        }
        html.push_str("</body></html>");
        write_html(destination, &html)
    }

    /// Export the current invoice and party details to a new, printable HTML file.
    /// No tax compliance, historical master snapshot, or payment status is implied.
    pub fn export_invoice_html(
        &self,
        company_id: i64,
        invoice_id: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        crate::opening_bills::ensure_trade_invoice(&self.conn, company_id, invoice_id)?;
        let destination = html_destination(destination.as_ref())?;
        let company = self
            .companies()?
            .into_iter()
            .find(|c| c.id == company_id)
            .ok_or_else(|| Error::Validation("Company not found".into()))?;
        let invoice = self
            .invoices(company_id)?
            .into_iter()
            .find(|i| i.id == invoice_id)
            .ok_or_else(|| Error::Validation("Invoice not found in the selected company".into()))?;
        let draft = self.invoice_draft(company_id, invoice_id)?;
        let profile = self.ledger_profile(company_id, draft.party_ledger_id)?;
        let business = self.company_profile(company_id)?;
        let mut html = printable_html("Invoice record");
        let _ = write!(html, "<h1>{}</h1><p>", escape(&company.name));
        for value in [
            &business.address,
            &business.region,
            &business.postal_code,
            &business.country,
        ] {
            if !value.trim().is_empty() {
                let _ = write!(html, "{}<br>", escape(value));
            }
        }
        for (label, value) in [
            ("Email", &business.email),
            ("Phone", &business.phone),
            ("Tax ID", &business.tax_id),
            ("Registration type", &business.registration_type),
        ] {
            if !value.trim().is_empty() {
                let _ = write!(html, "{}: {}<br>", label, escape(value));
            }
        }
        let document_title = match invoice.entry_mode {
            crate::inventory::InvoiceEntryMode::Invoice => {
                format!("{} invoice", invoice.kind.as_str())
            }
            crate::inventory::InvoiceEntryMode::Voucher => invoice.document_type().to_owned(),
        };
        let _ = write!(
            html,
            "</p><p><strong>{} record</strong><br>Reference: {}<br>Date: {}",
            document_title,
            escape(&invoice.reference),
            escape(&invoice.date)
        );
        if invoice.bill_reference != invoice.reference {
            let _ = write!(
                html,
                "<br>Bill reference: {}",
                escape(&invoice.bill_reference)
            );
        }
        if let Some(number) = &invoice.external_reference.number {
            let _ = write!(html, "<br>External invoice number: {}", escape(number));
        }
        if let Some(date) = &invoice.external_reference.date {
            let _ = write!(html, "<br>External invoice date: {}", escape(date));
        }
        for bill in &invoice.bill_splits {
            let _ = write!(
                html,
                "<br>Bill {}: {}",
                escape(&bill.reference),
                format_amount(bill.amount)
            );
            if let Some(date) = &bill.due_date {
                let _ = write!(html, " · Due {}", escape(date));
            }
        }
        if let Some(date) = &invoice.due_date {
            let _ = write!(html, "<br>Due: {}", escape(date));
        }
        if let Some(days) = invoice.credit_days {
            let _ = write!(html, "<br>Credit period: {days} days");
        }
        for id in &invoice.stock_note_ids {
            let note = get_note(&self.conn, company_id, *id)?;
            let _ = write!(
                html,
                "<br>{} {} ({})",
                note_label(note.kind),
                escape(&note.reference),
                escape(&note.date)
            );
        }
        if let Some(reversal) = &invoice.reversal {
            let _ = write!(
                html,
                "<br><strong>Reversed on {} by note {}</strong>",
                escape(&reversal.date),
                escape(&reversal.reference)
            );
        }
        html.push_str("</p><p><strong>Party</strong><br>");
        let _ = write!(html, "{}", escape(&invoice.party));
        for value in [
            &profile.mailing_name,
            &profile.address,
            &profile.region,
            &profile.postal_code,
            &profile.country,
        ] {
            if !value.trim().is_empty() {
                let _ = write!(html, "<br>{}", escape(value));
            }
        }
        if !profile.tax_id.trim().is_empty() {
            let _ = write!(html, "<br>Tax ID: {}", escape(&profile.tax_id));
        }
        html.push_str("</p><table><thead><tr><th>Item</th><th class=\"num\">Quantity</th><th class=\"num\">Price</th><th class=\"num\">Discount</th><th class=\"num\">Net</th></tr></thead><tbody>");
        for line in &invoice.lines {
            let discount = match &line.discount {
                InvoiceDiscount::None => "—".to_owned(),
                InvoiceDiscount::Fixed(_) => format_amount(line.discount_amount),
                InvoiceDiscount::Percent(rate) => {
                    format!("{}% ({})", rate, format_amount(line.discount_amount))
                }
            };
            let _ = write!(html, "<tr><td>{}</td><td class=\"num\">{} {}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>", escape(&line.item), format_quantity(i128::from(line.quantity)), escape(&line.unit), escape(&line.price_label()), escape(&discount), format_amount(line.amount));
        }
        let subtotal: i128 = invoice
            .lines
            .iter()
            .map(|line| i128::from(line.amount))
            .sum();
        html.push_str("</tbody></table><table class=\"totals\"><tbody>");
        let _ = write!(
            html,
            "<tr><td>Stock items after discounts</td><td class=\"num\">{}</td></tr>",
            money(subtotal)
        );
        for tax in &invoice.taxes {
            let ledger = if tax.source_rates.is_empty() {
                tax.ledger.clone()
            } else {
                format!(
                    "{} (source rates {})",
                    tax.ledger,
                    crate::taxes::format_source_tax_rates(&tax.source_rates)
                )
            };
            let _ = write!(
                html,
                "<tr><td>Tax in {}</td><td class=\"num\">{}</td></tr>",
                escape(&ledger),
                format_amount(tax.amount)
            );
        }
        for adjustment in &invoice.adjustments {
            let _ = write!(
                html,
                "<tr><td>{} {}</td><td class=\"num\">{}</td></tr>",
                escape(&adjustment.ledger),
                escape(&adjustment.description),
                format_amount(adjustment.amount)
            );
        }
        let _ = write!(
            html,
            "<tr class=\"total\"><td>Total</td><td class=\"num\">{}</td></tr></tbody></table>",
            money(invoice.total)
        );
        if let Some(calculated) = &invoice.tax_calculation {
            let _ = write!(
                html,
                "<p>Taxable amount {} · Tax profile {}, version {} (effective {})</p>",
                format_amount(calculated.calculation.taxable_amount),
                escape(&calculated.profile),
                calculated.calculation.version_id,
                escape(&calculated.effective_on)
            );
        }
        if !draft.narration.is_empty() {
            let _ = write!(
                html,
                "<p><strong>Notes</strong><br>{}</p>",
                escape(&draft.narration)
            );
        }
        html.push_str("<p class=\"note\">Basic invoice record. Company and party details reflect their current profiles. Review before use; this layout does not establish tax or statutory compliance.</p></body></html>");
        write_html(destination, &html)
    }

    /// Export a printable payroll record through the selected reporting cutoff.
    pub fn export_pay_statement_html(
        &self,
        company_id: i64,
        accrual_id: i64,
        through: Option<&str>,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = html_destination(destination.as_ref())?;
        let company = self
            .companies()?
            .into_iter()
            .find(|company| company.id == company_id)
            .ok_or_else(|| Error::Validation("Company not found".into()))?;
        let accrual = self
            .payroll_register(company_id, None, through, None)?
            .into_iter()
            .find(|row| row.id == accrual_id)
            .ok_or_else(|| {
                Error::Validation("Payroll entry not found through the selected date".into())
            })?;
        let lines = self.payroll_lines(company_id, accrual_id)?;
        let mut html = printable_html("Pay statement");
        let _ = write!(
            html,
            "<h1>{}</h1><p><strong>Pay statement</strong><br>Employee {} · {}",
            escape(&company.name),
            escape(&accrual.code),
            escape(&accrual.employee)
        );
        if !accrual.department.is_empty() {
            let _ = write!(html, "<br>Department {}", escape(&accrual.department));
        }
        let _ = write!(
            html,
            "<br>Period {} to {}<br>Accrual reference {} · Posted {}<br>Voucher {}</p>",
            escape(&accrual.period_start),
            escape(&accrual.period_end),
            escape(&accrual.reference),
            escape(&accrual.date),
            accrual.voucher_id
        );
        if let Some(reversal) = &accrual.reversal {
            let _ = write!(
                html,
                "<p><strong>Accrual reversed {}</strong><br>Voucher {} · {}</p>",
                escape(&reversal.date),
                reversal.voucher_id,
                escape(&reversal.reference)
            );
        }
        html.push_str("<table><thead><tr><th>Pay head</th><th>Type</th><th class=\"num\">Amount</th></tr></thead><tbody>");
        for line in lines {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
                escape(&line.head),
                line.kind.label(),
                format_amount(line.amount)
            );
        }
        html.push_str("</tbody></table><table class=\"totals\"><tbody>");
        for (label, amount) in [
            ("Gross earnings", i128::from(accrual.gross)),
            ("Deductions", i128::from(accrual.deductions)),
            ("Net accrued", i128::from(accrual.net)),
            (
                "Employer contributions",
                i128::from(accrual.employer_contributions),
            ),
            ("Paid", accrual.paid),
            ("Outstanding", accrual.outstanding),
        ] {
            let _ = write!(
                html,
                "<tr><td>{label}</td><td class=\"num\">{}</td></tr>",
                money(amount)
            );
        }
        html.push_str("</tbody></table>");
        if let Some(date) = through {
            let date = crate::parse_date(date)?;
            let _ = write!(html, "<p>Payment and reversal status through {date}.</p>");
        } else {
            html.push_str("<p>Payment and reversal status includes all saved dates.</p>");
        }
        html.push_str(
            "<p class=\"note\">Pay statement from recorded payroll amounts.</p></body></html>",
        );
        write_html(destination, &html)
    }
}
