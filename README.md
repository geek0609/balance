# Optimus Prime

Optimus Prime is an independent, open-source Rust accounting application for Windows. It is an early prototype, not a production accounting system; do not use it for statutory reporting or irreplaceable business records.

## Running the prototype

Install a Rust toolchain and run `cargo run` on Windows. The application stores its own SQLite database in the user's local application-data directory. Create a company, add at least two ledgers, then post a balanced two-line journal voucher and view the reports.

## Status

The prototype supports company and ledger creation, simple journal vouchers, and all-time trial balance, profit and loss, and balance sheet summaries. Imports, opening balances, tax, voucher editing, and statutory reports are not implemented. See the [roadmap](docs/roadmap.md) for planned work.

Licensed under Apache-2.0; see [LICENSE](LICENSE).
