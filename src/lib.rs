//! `ledgerlab` — a double-entry ledger that proves its own invariants and says **where** it
//! started to disagree with somebody else's statement.
//!
//! Three ideas, and the code is a consequence of them:
//!
//! 1. **money is exact**: `i64` micro-units, no floats anywhere, and parsing refuses what it
//!    cannot represent instead of rounding it away ([`money`]);
//! 2. **the log is the truth**: append-only, durable on write, and balances are a cache rebuilt
//!    from it. Replay is not a debugging feature, it is how the state exists ([`ledger`], [`log`]);
//! 3. **correctness is checked, not asserted**: [`audit`] recomputes the balances from the
//!    transactions and compares, and [`reconcile`] reports the first entry where an external
//!    statement and the ledger disagree, with the reason.
//!
//! Every claim in the README is reproducible with one command, and the commands are
//! `open`, `apply`, `balances`, `verify`, `reconcile`, `snapshot`, `compact`, `report`.

pub mod audit;
pub mod ledger;
pub mod log;
pub mod money;
pub mod reconcile;

pub use audit::{audit, Audit, Violation, ViolationKind};
pub use ledger::{Account, AccountKind, Applied, Event, Ledger, LedgerError, Posting, Transaction};
pub use log::{compact, Log, LogError};
pub use money::{Amount, MoneyError};
pub use reconcile::{reconcile, Divergence, Reconciliation, ReconciliationReason, StatementEntry};
