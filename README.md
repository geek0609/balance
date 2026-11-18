# Balance

Balance is an open-source accounting and business-management application written in Rust. Its long-term goal is to cover the complete range of workflows needed by businesses to move their existing books, inventory and operations into Balance.

**Status: early prototype.** Features listed as available are partial and have not all been verified. Do not use the application for production accounting, statutory filing or irreplaceable records yet. The planned list is a goal, not a claim that the functionality or regulatory compliance already exists.

## Run the prototype

On Windows, install a Rust toolchain and run `cargo run`. Balance stores a SQLite database in the user's local application-data directory. Create a company, create ledgers in account groups, and enter balanced vouchers. The application is licensed under [Apache-2.0](LICENSE).

To open an existing database at a different absolute path, run `cargo run -- --database "C:\path\to\balance.db"`. The selected path is shown in the application header. The optional path must already exist; the default database is used when the option is omitted.

## Feature status at a glance

| Area | Available now | Planned coverage |
| --- | --- | --- |
| Company and accounts | Company selection, account groups, ledgers, profiles, opening balances and a date cutoff | Company groups, periods, consolidation and richer masters |
| Accounting and reports | Balanced vouchers, basic financial reports and cost centres | Advanced allocations, full voucher workflows, budgeting and audit |
| Sales and purchasing | Stock orders, invoicing, settlements and full reversals | Quotes, delivery and receipt notes, returns, advanced pricing and complete billing |
| Inventory | Stock movements, counts, standard costing and weighted average valuation | More valuation methods, integrated cost postings, advanced tracking and manufacturing |
| Tax and payroll | Manual invoice tax amount | Jurisdiction-specific tax and payroll workflows |
| Banking | Payment and receipt vouchers, manual clearance and restricted statement matching | Broader statement formats, instruments and integrations |
| Data exchange | Restricted XML import, database snapshots and restore to a new file | Guided migration, reconciliation and APIs |
| Administration | Single-user local prototype | Permissions, security, concurrent access and recovery |

## Available in the prototype

These capabilities are implemented at a basic level; their coverage, usability and correctness still need comprehensive verification.

- **Companies and accounts:** create and select companies; save company address, contact and registration details, plus informational financial-year and books-start dates, with previous company details retained; create hierarchical account groups and ledgers; rename ledgers or move them to a group in the same accounting category, with earlier names and groups retained; save ledger mailing, address, contact and registration details; maintain optional party credit-day defaults and informational credit limits with prior values retained; enter and revise non-zero opening balances against an automatically maintained opening-difference account, or clear one with a dated counter-entry and reason. Credit-day defaults can be explicitly applied to invoices; limits are not enforced. Reports use current ledger names and groups. Financial year and books start dates are informational. A separate company cutoff locks transaction dates on or before the selected day. Setting, changing or removing the cutoff requires a reason and records the change in company history.
- **Period closing:** the company cutoff blocks new vouchers and stock movements dated in the locked period, including invoices, settlements, opening balances, adjustments, transfers, stock counts and imported entries. Existing voucher edits cannot move entries into or out of the locked period. Later counter-entries and settlements can still refer to earlier vouchers under the rules for those workflows. Ledger and inventory masters, company details and bank reconciliation are outside the transaction lock. Any user of the local prototype can change the cutoff.
- **Accounting entries:** create multi-line balanced vouchers; label them as journal, payment, receipt, transfer, sales, purchase, credit note or debit note; record dates, references and narration; browse and edit ordinary vouchers; retain prior voucher versions; reverse an ordinary voucher with a separate dated journal entry and reason. The original and its reversal then cannot be edited. Voucher labels alone do not produce stock or tax effects; linked invoice, settlement and opening-balance entries use their own workflows.
- **Accounting reports:** a searchable day book with date and voucher-type filters; ledger activity with opening, running and closing balances; a sales and purchase invoice register filtered by date, type, party or search; date-filtered trial balance with debit, credit and difference totals, account-group trial balance, profit and loss, and balance-sheet summaries. Group totals include descendant groups and use current ledger classifications. Export the filtered day book, ledger activity, invoice register, trial balances, outstanding invoices and party outstanding summaries to new CSV files. The invoice register shows original totals and includes settlements and reversals from all saved dates. Outstanding reports support an as-of date.
- **Cost centres:** create company-specific centres and subcentres. Split a saved income or expense posting across centres, with any remainder left unallocated. Changes require a reason and retain the saved splits in history. Reverse ordinary vouchers or invoices to carry their allocations into the counter-entry. Review or export income, expenses and net amounts for a date range, with direct figures, totals including subcentres, unallocated amounts and the company total. Reports use current allocations. Period locks protect allocation changes. Clear a voucher’s allocations before editing its postings. Opening entries and balance sheet postings remain outside the allocation workflow.
- **Bank clearance:** mark individual asset-ledger postings with a clearance date on or after the voucher date, review the book, cleared and uncleared totals as of a date, and retain a history of changes. Import a restricted CSV statement and manually match one statement entry to one posting with the same amount; matching uses the statement date as the clearance date and records match changes. A unique eligible posting can be suggested, but must be reviewed and matched manually. Enter a statement opening balance to calculate a statement balance from imported entries and compare it with the books. A match or clearance must be removed before its voucher can be edited; past reports use current clearance dates and opening balances.
- **Stock:** create simple units with up to three decimal places, hierarchical stock groups, items with descriptions, locations and item batches with optional manufacture and expiry dates; edit item names, groups and descriptions with previous versions retained; maintain append-only effective-dated base-unit price lists and explicitly fill blank invoice prices for the invoice date; set per-item reorder levels and fixed suggested quantities with prior settings retained; view company-wide low-stock warnings; record quantity-only adjustments and transfers; allocate invoice items to locations and optional batches; view balances and dated movement history across all locations or for one location, plus batch quantities and expiry status for a selected date. Low-stock warnings use current rules, ignore pending orders and do not create purchase orders. The invoice stores the price used but not a link to the list. Existing unbatched movements stay unbatched. Item base units cannot be changed. Quantities can go negative, and adjustments and transfers do not create accounting entries.
- **Standard costing:** set an effective-dated standard cost per base unit with a recorded reason and history. Use the latest rate for a date to value company-wide stock on hand. Report item sales after discounts against the standard rate on the original invoice date, with reversed sales offset at the same rate on the note date. Both reports export to CSV and separate unknown costs from known figures. New rates in locked periods are blocked. Reports use the currently saved rates, exclude tax and adjustments from item-sales margin, and do not book inventory value or cost of sales into the ledgers.
- **Weighted average valuation:** value stock across all company locations using net purchase item amounts and recorded acquisition costs for positive quantity adjustments, such as opening quantities and count gains. Each receipt adds to the item’s units and value. Issues release a proportion of the available value. On the same date, the report processes receipts and restores stock from earlier reversed sales before issues. Invoices fully reversed on their original date have no net effect. Later sales reversals restore the original issue cost. Purchase reversals issue stock at the then-current average. Missing receipt costs and insufficient stock leave the item’s valuation unavailable and identify the first issue. Acquisition cost changes retain their history and respect period locks. This method excludes taxes, freight and other non-item charges, reports value by item rather than warehouse or batch, and makes no ledger postings.
- **Physical stock counts:** record quantities for selected items and batches at one location and date. Preview book quantities and discrepancies, then post the count and any quantity adjustments as one transaction. The app asks for a new preview if a book quantity changes. Count history retains book quantities, observations and linked adjustments, including items whose counts matched. The book quantities include movements already saved through the count date. Later backdated entries can change reported balances without altering the count record. Counts are limited to the lines entered, and adjustments do not post accounting entries or value the stock.
- **Sales and purchase orders:** record orders for stock items with a party, reference, order date, expected date and agreed unit prices. Filter the order register by type, status, reference or party. Prepare an invoice for remaining quantities and adjust the draft for a partial fulfilment. The invoice, its voucher and stock movements, and the order link save together. Each invoice can fulfil one order, and an order can be fulfilled by several invoices. Close remaining quantities with a recorded reason. Orders do not reserve stock or affect the books until invoiced. Linked invoices and saved order details cannot yet be edited. The register includes all linked invoices regardless of date, and invoice prices can differ from the agreed order prices.
- **Invoices and settlement:** create basic sales and purchase invoices with items, quantities, prices, fixed or percentage item discounts, optional per-item accounting ledgers, multiple location or batch allocations per item, an optional due date or credit period in days, a manually entered tax amount, and separate manually entered service, charge or deduction lines; invoices without stock items require a positive service or charge line in a compatible account. Create the related voucher and any stock movements atomically; review invoices; prepare a new draft from an existing invoice with a new date and reference; export a basic printable HTML invoice record to a new file; edit invoices without settlement history with prior versions retained; allocate a partial receipt or payment across one or more invoices of the same party and type; reverse an entire receipt or payment with a separate voucher and reason; view outstanding invoices, party summaries and ageing buckets from the invoice or due date as of a chosen date. Copying retains prices, tax and allocations for review. Percentage discounts apply to the rounded gross line amount. Past reports and exported documents use current invoice and party details.
- **Full invoice reversals:** post a dated credit note for a sale or debit note for a purchase. The note reverses the original postings and reverses stock quantities at their original locations and batches. The app retains the invoice and note, marks the invoice as reversed and closes its outstanding amount from the note date. Reversing a linked invoice restores unfulfilled quantities on its order unless that order was explicitly closed. Replacement invoices cannot predate an existing reversal on the order. Reverse any active settlements before reversing the invoice. Partial returns, refunds, replacement stock allocations and statutory note documents are planned.
- **Data and continuity:** preview and atomically import a restricted subset of XML, including supported account groups, ledgers, contact fields, opening balances, voucher lines, record identifiers, simple units, stock groups, flat locations, items, unvalued opening stock quantities, narrowly supported item and non-stock invoices, and simple invoice settlements. Detected unsupported records and fields block the import. Create a SQLite snapshot of all companies or restore a backup to a new file, with database integrity and reference checks and schema upgrades on the restored copy.
- **Interface:** Windows desktop interface with a navigation menu, scrollable screens and `Ctrl+Enter` for saving an ordinary voucher.

### Current limits

- XML import needs verification against representative exports. Invoices require one matching new bill reference; item invoices also need one accounting allocation per item and matching quantities and amounts. Non-stock invoices require a positive posting to a compatible income, expense or asset account. Separate percentage or fixed item discount fields are accepted when the calculated net amount matches; combined discounts and fixed discounts across multiple stock allocations are unsupported. Multiple location or batch allocations are accepted only when each allocation's discounted amount matches its quantity and price and their rounded amounts sum to the item amount. Additional ledger postings are retained as unclassified service, charge or deduction lines, with no tax treatment inferred. Simple receipts or payments may allocate against available invoices for one party, using one asset ledger and matching totals; an included credit period must match the invoice. Credit periods expressed in whole days are retained. Unvalued opening quantities may be allocated to locations and batches with manufacture dates; unallocated quantities use the default location. Other payment-term forms, tax details, expiry dates, invoice batch dates, nested locations, advances and other bill allocations are not supported. Orders, order links, physical stock counts, invoice-linked return or reversal records, other inventory transactions, opening stock valuation, statutory tax information and many account and transaction fields are also unsupported. A clean preview does not establish that an entire business dataset has been migrated correctly.
- Cost-of-goods ledger postings, automatic tax calculations, tax filing and statutory compliance are not implemented. Weighted average valuation reports a retrievable amount only for items with a complete and consistent history under its documented costing rules. Invoice tax amounts are manual; invoices with settlement history cannot yet be edited, including after reversal. A reversal applies to every allocation on its settlement voucher; individual allocations cannot yet be reversed independently. The invoice list includes future-dated settlements, settlement reversals and invoice reversals; use the outstanding report for an as-of view. New settlements cannot be dated before an existing reversal on the same invoice. A new or revised opening balance cannot predate the latest clearing for that ledger.
- Backup, restore, invoice document export and statement import need runtime verification; a failed backup, restore or document export may leave an incomplete destination file. The invoice document is a basic record for review and printing, not a statutory tax layout; it does not show payment status, and company and party details are taken from their current profiles. Restoring creates a separate database and does not replace the one currently in use. Automatic bank matching, many bank statement formats, multi-user access, permissions, comprehensive audit controls, payroll, manufacturing and most advanced reports are not implemented.
- This is not a finished or verified migration tool. Keep original records and independently reconcile all data and reports before relying on any future migration.
- The opening-difference ledger name is reserved for the automatically maintained control account. Imports containing another ledger with that name require resolution before they can proceed.

For the restricted bank statement import, prepare a UTF-8 CSV with columns `id,date,description,amount` in that order. IDs must be stable and unique within the selected ledger, dates use `YYYY-MM-DD`, deposits are positive, withdrawals are negative, and amounts have at most two decimal places. The statement opening balance is entered separately; calculated balances depend on the imported entries being complete and are not independently verified. Files are limited to 10 MiB and 10,000 entries.

Report CSV exports include the company and selected date filters. Day-book, invoice-register, trial-balance, outstanding and company-wide stock files contain a `context` row even when no entries match; ledger files contain opening and closing rows. Group trial-balance parent totals include their descendants, so parent and child totals must not be added together. The stock quantity export uses current reorder settings even for a historical quantity date and excludes pending orders and valuation. The stock valuation exports show standard cost and weighted average values separately. Debit and credit columns are positive; ledger balances use debit-positive, credit-negative signs. Text that could be interpreted as a spreadsheet formula is prefixed with an apostrophe, which changes those exported text values. Exported reports and spreadsheet handling still need runtime verification.

An open-ended report or stock quantity view includes all saved entries, including entries dated in the future. Supply an end or as-of date when a cutoff is needed.

## Planned feature coverage

The roadmap covers the breadth required for a mature, full-featured accounting and business-management system. Individual items may require substantial design, implementation and verification; the list will be expanded as remaining workflows and edge cases are identified.

### Company and financial setup

- Company profiles, addresses, contact details, financial years, books start dates and configurable accounting periods.
- Multiple companies, company groups, consolidation, inter-company transactions, branch management and separate registrations.
- Company-specific features, numbering, currency, units, defaults, document series and accounting preferences.
- Financial-year transitions, opening a new period, splitting or archiving historical company data and carrying forward balances with reconciliation.
- Opening balances for accounts, inventory, bills and other masters; reconciliation of opening differences; year-end closing, rollover and period locks.

### Chart of accounts and accounting

- Complete account-group hierarchy, custom groups and subgroups, aliases, ledger classifications, multiple addresses and richer account master details.
- Cash, bank, customer, supplier, income, expense, asset, liability and capital accounts; account histories, statements and drill-down.
- Journal, payment, receipt, transfer, sales, purchase, credit-note and debit-note workflows, including recurring, reversing, optional, post-dated and cancelled entries.
- Configurable voucher types, numbering schemes and series, references, attachments, narration, approval, editing, reversal and cancellation.
- Multi-line entries, allocations, account reconciliation, error correction, exception handling and complete audit trails.
- Cost centres and categories, cost allocation, job and project costing, budgets, scenarios, projections, variance analysis and cash-flow planning.
- Account and transaction-level controls, configurable entry validation, correction workflows and period-wise comparisons.
- Multiple currencies, exchange rates, foreign-currency transactions, revaluation and exchange gain or loss.

### Sales, purchasing and invoicing

- Customer and supplier masters, delivery and billing addresses, contacts, terms, tax registrations, credit limits and price arrangements.
- Quotations, sales and purchase orders, order fulfilment, partial fulfilment, pre-closure and order tracking.
- Sales and purchase invoices for goods and services, accounting-only invoices, discounts, charges, freight, rounding and configurable document layouts.
- Delivery and receipt notes, tracking references, returns, rejection notes, credit and debit notes, and links between orders, deliveries and invoices.
- Invoice printing, previews, PDF and other exports, sharing, attachments, branding, templates and configurable fields.
- Point-of-sale workflows, price lists, multiple price levels, customer-specific pricing and promotions.

### Receivables, payables and banking

- Bill-by-bill receivables and payables, outstanding balances, part payments, advances, credit notes, multi-invoice allocations and settlement reversal.
- Ageing, overdue analysis, payment terms, credit control, interest calculations, statements and reminders.
- Bank and cash books, deposits, withdrawals, transfers, cheque management and cheque printing.
- Bank statement import, automated and manual reconciliation, matching rules, exceptions, bank charges and reconciliation reports.
- Payment instruments and integrations with supported payment and banking services, subject to provider availability.

### Inventory and manufacturing

- Stock groups, categories, items, services, units of measure, alternate units, unit conversions, barcodes and item descriptions.
- Multiple warehouses and locations, stock transfers, inward and outward movements, adjustments, physical stock counts and reconciliation.
- Batch, lot, serial number, expiry and manufacturing-date tracking where applicable; inventory allocation and traceability.
- Opening stock, stock valuation methods, cost tracking, additional purchase costs, cost of goods, gross profit and integrated accounting.
- Reorder levels, reorder quantities, stock ageing, negative-stock controls, availability and demand analysis.
- Bills of materials, production, consumption, by-products, manufacturing journals, job work, material in/out and job costing.
- Order-wise stock reservations, tracking of goods in transit and allocation of stock across locations and documents.
- Inventory-only and accounting-linked transactions, separate inventory and accounting finalisation, and complete stock history.

### Taxation and statutory workflows

- Configurable company and party registrations, effective dates, tax classifications, rates, exemptions and jurisdiction-specific rules.
- Indian GST support including HSN/SAC, taxable values, rates, input/output tax, adjustments, returns, reconciliation and multiple registrations.
- Electronic invoices, electronic waybills and related portal workflows where applicable and supported.
- Tax deducted and collected at source, withholding, related certificates, returns and reconciliation.
- Payroll-related statutory deductions and employer obligations, including applicable provident fund, insurance and professional-tax workflows.
- Historical and regional indirect-tax workflows where relevant, plus jurisdiction-specific compliance for supported regions, including the Gulf region, Kenya and Nepal.
- Rule updates, auditability and validation against current official requirements for each supported jurisdiction. No statutory compliance is claimed until separately implemented and verified.

### Payroll and workforce

- Employee masters, employee groups, departments, designations, attendance, shifts, leave and production-based attendance.
- Salary structures, pay heads, earnings, deductions, reimbursements, loans, advances and payroll processing.
- Payslips, payroll registers, salary payments, employer contributions, statutory records and payroll reporting.

### Reports, analysis and verification

- Detailed trial balance, balance sheet, profit and loss, cash flow, fund flow, ratio analysis and configurable financial statements.
- Day books, journals, cash and bank books, account registers, ledger statements, voucher registers and exception reports.
- Sales, purchase, invoice, order, profitability, receivables, payables, ageing and collection reports.
- Stock summary, movement, valuation, ageing, warehouse, batch, reorder, production and cost reports.
- Tax, payroll, audit, budget, cost-centre and management reports, with date ranges, drill-down, comparison, filtering and saved views.
- Custom report layouts, printing and export; verification, anomaly detection and reconciliation tools.
- Comparative and period-wise reporting, detailed and condensed views, configurable columns and drill-through from summaries to underlying entries.

### Data migration, exchange and integrations

- Guided, repeatable migration of companies, masters, opening balances, transactions, inventory, invoices, allocations, tax, payroll and historical records.
- Preservation of source identifiers, numbering, relationships, audit data, unsupported fields and migration diagnostics; duplicate detection and resumable imports.
- Pre-import previews, validation, explicit mappings, transactional application, post-import reconciliation and side-by-side report comparison.
- XML, JSON, CSV and spreadsheet import/export where appropriate, including documented interchange formats and configurable field mappings.
- Programmatic APIs and supported database or data-exchange interfaces; integrations with other business applications without silently changing existing data.

### Security, reliability and administration

- Multiple users, roles, permissions, approval flows, activity monitoring and configurable access to companies and reports.
- Complete, tamper-evident edit and activity logs for all relevant records, with attributable actions and retention controls.
- Local and network deployment, concurrent access, locking, conflict handling, recovery, data integrity checks and scalable performance.
- Verified backups, restore, disaster recovery, data repair, encryption and secure credential handling.
- Remote access, synchronisation and branch connectivity with appropriate authentication, authorisation and conflict handling.

### Experience, extensibility and delivery

- Fast keyboard-first workflows, shortcuts, accessible navigation, search, context help, guided setup and consistent error handling.
- Localisation, multilingual entry and reports, configurable dates, numbers, currencies and units.
- Custom fields, forms, reports, document templates, automation and a documented extension system.
- Configurable printing and sharing preferences, reusable document layouts, data-entry defaults and user-specific display settings.
- Stable Windows releases, installation and updates; portable core and support for additional desktop platforms where validated.
- Automated and manual verification, representative migration fixtures, performance testing, documentation and a clear compatibility matrix.

## Contributing

See [the roadmap](docs/roadmap.md) for the current development sequence. Contributions should keep data handling explicit, preserve existing records during upgrades and include appropriate verification when testing resumes.
