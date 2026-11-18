//! Printable invoice, quotation and payroll records.
use crate::{
    format_amount,
    inventory::{format_quantity, line_amount, InvoiceDiscount},
    quotations::quotation,
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

impl Store {
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
        let _ = write!(
            html,
            "</p><p><strong>{} invoice record</strong><br>Reference: {}<br>Date: {}",
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
            "<tr><td>Stock items after discounts</td><td class=\"num\">{}</td></tr>",
            money(subtotal)
        );
        for tax in &invoice.taxes {
            let _ = write!(
                html,
                "<tr><td>Tax in {}</td><td class=\"num\">{}</td></tr>",
                escape(&tax.ledger),
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
