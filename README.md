# Optimus Prime

Optimus Prime is an independent, open-source Rust accounting application for Windows. It is an early prototype, not a production accounting system; do not use it for statutory reporting or irreplaceable business records.

## Running the prototype

Install a Rust toolchain and run `cargo run` on Windows. The application stores its own SQLite database in the user's local application-data directory. Create a company, add at least two ledgers, then post a balanced journal voucher and view the reports. Existing vouchers can be reviewed and corrected from the voucher screen.

## Status

The prototype supports company creation, hierarchical account groups and ledgers, ledger contact details, opening balances, categorized multi-line vouchers, voucher history and editing, date-filtered ledger activity, trial balance, profit and loss, and balance sheet summaries. Opening entries use an automatically maintained difference account, which should be reconciled after all opening balances are entered. It also includes stock quantities, basic sales and purchase invoices, and partial receipts and payments allocated to invoices. Invoice creation records accounting postings and quantity movements together; settlement records an associated accounting voucher. Stock valuation and cost of goods are not calculated; tax amounts must be entered manually and statutory rules are not implemented. Voucher categories are labels and do not automatically calculate tax or stock movements. Each edit retains the previous voucher and its postings; edits made before this feature do not have a recorded history. Clearing opening balances, invoice and settlement editing, and statutory reports are not implemented. See the [roadmap](docs/roadmap.md) for planned work.

The initial XML importer supports a limited set of simple accounting records and selected ledger contact fields, and blocks imports containing detected unsupported data. It must be verified with representative exports before using it to migrate business records. It currently requires supported records to include their account groups, unambiguous ledger names, and voucher numbers; it does not accept files containing unsupported records or fields. Preview shows issues before any data is written, and applying a preview writes the entire set in one database transaction.

The Back up screen can create a SQLite snapshot of all companies at a new, absolute file path. Restore from within the application is not implemented, and the backup workflow has not yet been verified.

Licensed under Apache-2.0; see [LICENSE](LICENSE).
