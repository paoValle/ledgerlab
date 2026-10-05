//! Reconciliation: comparing this ledger with somebody else's statement, and saying **where**
//! they started to disagree.
//!
//! Every tool can report a difference. Almost none reports *where it began*: "the bank statement
//! is 0.45 short" is a puzzle, "entry 4 (`fee_1`) is where they diverge, and the ledger booked 1.05
//! where the bank charged 1.50" is an answer.
//!
//! The comparison is **positional**, on two sequences that are both in chronological order: the
//! ledger's referenced postings on one account, and the statement's entries. That is a real
//! assumption, and it is the reason the three reasons below exist instead of one:
//!
//! - [`ReconciliationReason::AmountMismatch`] — same reference, different amount: a real
//!   divergence, and the one that costs money;
//! - [`ReconciliationReason::PendingInLedger`] — the ledger has an entry the statement does not:
//!   usually a timing difference (money that has not cleared), so it must not be reported as an
//!   error with the same weight;
//! - [`ReconciliationReason::MissingFromLedger`] — the statement has an entry the ledger does
//!   not: somebody else moved money, and this ledger has never heard of it.
//!
//! A single "delta" number collapses those three into a word that helps nobody.

use std::collections::BTreeSet;

use crate::ledger::Ledger;
use crate::money::Amount;

/// One line of somebody else's statement, signed from the point of view of the account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatementEntry {
    /// The external reference both sides should agree on.
    pub reference: String,
    /// The amount.
    pub amount: Amount,
}

/// Why the two sides stopped agreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationReason {
    /// Same reference, different amount.
    AmountMismatch,
    /// The ledger has an entry the statement does not: probably pending.
    PendingInLedger,
    /// The statement has an entry the ledger does not.
    MissingFromLedger,
    /// Both references exist on both sides, but not in the same order.
    OutOfOrder,
    /// One sequence is longer than the other.
    LengthMismatch,
}

impl ReconciliationReason {
    /// A short label for reports.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AmountMismatch => "amount mismatch",
            Self::PendingInLedger => "in the ledger, not in the statement (pending)",
            Self::MissingFromLedger => "in the statement, not in the ledger",
            Self::OutOfOrder => "same entries, different order",
            Self::LengthMismatch => "different number of entries",
        }
    }

    /// Whether this reason means the ledger is wrong, as opposed to merely behind.
    #[must_use]
    pub fn is_error(self) -> bool {
        matches!(self, Self::AmountMismatch | Self::MissingFromLedger)
    }
}

/// The first place the two sides disagree, with both running balances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Zero-based position in the sequence.
    pub index: usize,
    /// Why.
    pub reason: ReconciliationReason,
    /// The reference on the ledger side, if any.
    pub ledger_reference: Option<String>,
    /// The reference on the statement side, if any.
    pub statement_reference: Option<String>,
    /// Running balance of the ledger through the common prefix.
    pub ledger_running: Amount,
    /// Running balance of the statement through the common prefix.
    pub statement_running: Amount,
    /// `statement_running - ledger_running`.
    pub difference: Amount,
    /// One line a human can act on.
    pub detail: String,
}

/// The full result of a reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    /// The account compared.
    pub account: String,
    /// Referenced entries the ledger has on that account.
    pub ledger_entries: usize,
    /// Entries the statement has.
    pub statement_entries: usize,
    /// References present on both sides.
    pub matched: usize,
    /// Statement references the ledger has never seen.
    pub missing_from_ledger: Vec<String>,
    /// Ledger references the statement does not have.
    pub pending_in_ledger: Vec<String>,
    /// Where they started to disagree.
    pub first_divergence: Option<Divergence>,
    /// Final balance of the ledger side.
    pub ledger_final: Amount,
    /// Final balance of the statement side.
    pub statement_final: Amount,
}

impl Reconciliation {
    /// Whether the two sides agree, including the order.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.first_divergence.is_none()
    }

    /// The raw difference at the end, whatever the reason.
    #[must_use]
    pub fn final_difference(&self) -> Amount {
        self.statement_final
            .checked_add(self.ledger_final.negated())
            .unwrap_or(Amount::ZERO)
    }
}

/// Compares the ledger with a statement for one account.
///
/// Long on purpose: the sequence of comparisons *is* the algorithm, and splitting it into helpers
/// would hide the one thing a reader needs to check — that the first divergence is the first one.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn reconcile(ledger: &Ledger, account: &str, statement: &[StatementEntry]) -> Reconciliation {
    // the ledger side: transactions that touch this account and carry a reference, in order
    let mut side: Vec<(String, Amount)> = Vec::new();
    for (_, transaction) in ledger.entries() {
        let Some(reference) = transaction.reference.clone() else {
            continue;
        };
        let mut total = Amount::ZERO;
        let mut touched = false;
        for posting in &transaction.postings {
            if posting.account == account {
                touched = true;
                total = total.checked_add(posting.amount).unwrap_or(total);
            }
        }
        if touched {
            side.push((reference, total));
        }
    }

    let ledger_refs: BTreeSet<&str> = side
        .iter()
        .map(|(reference, _)| reference.as_str())
        .collect();
    let statement_refs: BTreeSet<&str> = statement
        .iter()
        .map(|entry| entry.reference.as_str())
        .collect();

    let missing_from_ledger: Vec<String> = statement
        .iter()
        .filter(|entry| !ledger_refs.contains(entry.reference.as_str()))
        .map(|entry| entry.reference.clone())
        .collect();
    let pending_in_ledger: Vec<String> = side
        .iter()
        .filter(|(reference, _)| !statement_refs.contains(reference.as_str()))
        .map(|(reference, _)| reference.clone())
        .collect();

    let mut ledger_running = Amount::ZERO;
    let mut statement_running = Amount::ZERO;
    let mut matched = 0_usize;
    let mut first_divergence: Option<Divergence> = None;

    let common = side.len().min(statement.len());
    for index in 0..common {
        let (ledger_reference, ledger_amount) = &side[index];
        let entry = &statement[index];

        if ledger_reference == &entry.reference {
            if *ledger_amount != entry.amount {
                first_divergence = Some(Divergence {
                    index,
                    reason: ReconciliationReason::AmountMismatch,
                    ledger_reference: Some(ledger_reference.clone()),
                    statement_reference: Some(entry.reference.clone()),
                    ledger_running,
                    statement_running,
                    difference: ledger_running
                        .checked_add(statement_running.negated())
                        .unwrap_or(Amount::ZERO),
                    detail: format!(
                        "{ledger_reference}: the ledger booked {ledger_amount}, the statement says {}",
                        entry.amount
                    ),
                });
                break;
            }
            matched += 1;
            ledger_running = ledger_running
                .checked_add(*ledger_amount)
                .unwrap_or(ledger_running);
            statement_running = statement_running
                .checked_add(entry.amount)
                .unwrap_or(statement_running);
            continue;
        }

        let reason = if !ledger_refs.contains(entry.reference.as_str()) {
            ReconciliationReason::MissingFromLedger
        } else if !statement_refs.contains(ledger_reference.as_str()) {
            ReconciliationReason::PendingInLedger
        } else {
            ReconciliationReason::OutOfOrder
        };
        first_divergence = Some(Divergence {
            index,
            reason,
            ledger_reference: Some(ledger_reference.clone()),
            statement_reference: Some(entry.reference.clone()),
            ledger_running,
            statement_running,
            difference: ledger_running
                .checked_add(statement_running.negated())
                .unwrap_or(Amount::ZERO),
            detail: format!(
                "at entry {index} the statement has {:?} ({}) where the ledger has {:?} ({})",
                entry.reference, entry.amount, ledger_reference, ledger_amount
            ),
        });
        break;
    }

    // if the common prefix agreed, a length difference is the divergence
    if first_divergence.is_none() && side.len() != statement.len() {
        first_divergence = Some(Divergence {
            index: common,
            reason: ReconciliationReason::LengthMismatch,
            ledger_reference: side.get(common).map(|(reference, _)| reference.clone()),
            statement_reference: statement.get(common).map(|entry| entry.reference.clone()),
            ledger_running,
            statement_running,
            difference: ledger_running
                .checked_add(statement_running.negated())
                .unwrap_or(Amount::ZERO),
            detail: format!(
                "the ledger has {} referenced entries on {account}, the statement has {}",
                side.len(),
                statement.len()
            ),
        });
    }

    // the final balances are totals, not running sums of a matched prefix: they are what a human
    // reconciles against a bank statement at the end of a period
    let ledger_final: Amount = side.iter().fold(Amount::ZERO, |total, (_, amount)| {
        total.checked_add(*amount).unwrap_or(total)
    });
    let statement_final: Amount = statement.iter().fold(Amount::ZERO, |total, entry| {
        total.checked_add(entry.amount).unwrap_or(total)
    });

    Reconciliation {
        account: account.to_owned(),
        ledger_entries: side.len(),
        statement_entries: statement.len(),
        matched,
        missing_from_ledger,
        pending_in_ledger,
        first_divergence,
        ledger_final,
        statement_final,
    }
}
