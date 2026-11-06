//! Simple printable documents built from saved invoice records.
use crate::{
    format_amount,
    inventory::{format_quantity, InvoiceDiscount},
    Error, Result, Store,
};
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

impl Store {
    /// Export the current invoice and party details to a new, printable HTML file.
    /// No tax compliance, historical master snapshot, or payment status is implied.
    pub fn export_invoice_html(
        &self,
        company_id: i64,
        invoice_id: i64,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let destination = destination.as_ref();
        if !destination.is_absolute()
            || !destination
                .extension()
                .is_some_and(|v| v.eq_ignore_ascii_case("html"))
        {
            return Err(Error::Validation(
                "Enter an absolute path to a new .html file".into(),
            ));
        }
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
        let mut html = String::from("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>Invoice record</title><style>body{font:14px system-ui,sans-serif;color:#182431;max-width:900px;margin:2rem auto;padding:0 1rem}h1{margin-bottom:.2rem}p{white-space:pre-line}table{border-collapse:collapse;width:100%;margin:1.5rem 0}th,td{text-align:left;border-bottom:1px solid #ccd3db;padding:.55rem;vertical-align:top}.num{text-align:right;white-space:nowrap}.totals{margin-left:auto;width:min(100%,360px)}.totals td{border:0}.total{font-weight:bold;border-top:2px solid #182431}.note{color:#4c5b68;font-size:12px}@media print{body{margin:0;max-width:none}}</style></head><body>");
        let _ = write!(
            html,
            "<h1>{}</h1><p><strong>{} invoice record</strong><br>Reference: {}<br>Date: {}",
            escape(&company.name),
            invoice.kind.as_str(),
            escape(&invoice.reference),
            escape(&invoice.date)
        );
        if let Some(date) = &invoice.due_date {
            let _ = write!(html, "<br>Due: {}", escape(date));
        }
        if let Some(days) = invoice.credit_days {
            let _ = write!(html, "<br>Credit period: {days} days");
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
            let _ = write!(html, "<tr><td>{}</td><td class=\"num\">{} {}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>", escape(&line.item), format_quantity(i128::from(line.quantity)), escape(&line.unit), format_amount(line.unit_price), escape(&discount), format_amount(line.amount));
        }
        let subtotal: i128 = invoice
            .lines
            .iter()
            .map(|line| i128::from(line.amount))
            .sum();
        html.push_str("</tbody></table><table class=\"totals\"><tbody>");
        let _ = write!(
            html,
            "<tr><td>Items after discounts</td><td class=\"num\">{}</td></tr>",
            money(subtotal)
        );
        if invoice.tax_amount != 0 {
            let _ = write!(
                html,
                "<tr><td>Tax (manual)</td><td class=\"num\">{}</td></tr>",
                format_amount(invoice.tax_amount)
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
        if !draft.narration.is_empty() {
            let _ = write!(
                html,
                "<p><strong>Notes</strong><br>{}</p>",
                escape(&draft.narration)
            );
        }
        html.push_str("<p class=\"note\">Basic invoice record. Party details reflect the current ledger profile. Review before use; this layout does not establish tax or statutory compliance.</p></body></html>");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|e| Error::Validation(format!("Cannot create invoice document: {e}")))?;
        file.write_all(html.as_bytes())
            .and_then(|_| file.flush())
            .map_err(|e| Error::Validation(format!("Cannot finish invoice document: {e}")))?;
        Ok(())
    }
}
