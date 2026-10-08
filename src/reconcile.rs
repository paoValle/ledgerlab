//! Reconciliation: comparing this ledger with somebody else's statement, and saying **where**
//! they started to disagree.
//!
//! Every tool can report a difference. Almost none reports *where it began*: "the bank statement
//! is 0.45 short" is a puzzle, "entry 4 (`fee_1`) is where they diverge, and the ledger booked 1.05
//! where the bank charged 1.50" is an answer.
//!
//! Entries are matched **by reference, not by position**: everything the two sides carry under one
//! reference is compared as a total, so a charge settled in two payouts (a split) and a payout that
//! covers several charges (a merge) reconcile when they add up. The matching step runs first and
//! keeps its own output ([`Settlement`]): which entries each reference consumed on each side, and
//! which were left over.
//!
//! Three reasons, because collapsing them into one "delta" helps nobody:
//!
//! - [`ReconciliationReason::AmountMismatch`] — same reference, different total: a real
//!   divergence, and the one that costs money;
//! - [`ReconciliationReason::PendingInLedger`] — the ledger has a reference the statement does
//!   not: usually a timing difference (money that has not cleared), so it must not be reported as
//!   an error with the same weight;
//! - [`ReconciliationReason::MissingFromLedger`] — the statement has a reference the ledger does
//!   not: somebody else moved money, and this ledger has never heard of it.
//!
//! References are walked in the ledger's order (the ledger is the truth here), with references the
//! ledger never saw appended in the order the statement gives them.

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
    /// Same reference, different total.
    AmountMismatch,
    /// The ledger has a reference the statement does not: probably pending.
    PendingInLedger,
    /// The statement has a reference the ledger does not.
    MissingFromLedger,
}

impl ReconciliationReason {
    /// A short label for reports.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AmountMismatch => "amount mismatch",
            Self::PendingInLedger => "in the ledger, not in the statement (pending)",
            Self::MissingFromLedger => "in the statement, not in the ledger",
        }
    }

    /// Whether this reason means the ledger is wrong, as opposed to merely behind.
    #[must_use]
    pub fn is_error(self) -> bool {
        matches!(self, Self::AmountMismatch | Self::MissingFromLedger)
    }
}

/// One reference the two sides share, and what it consumed on each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// The reference both sides carry.
    pub reference: String,
    /// Positions, in the ledger side, of the entries this reference covers.
    pub ledger_indexes: Vec<usize>,
    /// Positions, in the statement, of the entries this reference covers.
    pub statement_indexes: Vec<usize>,
    /// Sum of the ledger entries with this reference.
    pub ledger_total: Amount,
    /// Sum of the statement entries with this reference.
    pub statement_total: Amount,
}

impl Settlement {
    /// Whether this reference is present on both sides and adds up to the same total.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        !self.ledger_indexes.is_empty()
            && !self.statement_indexes.is_empty()
            && self.ledger_total == self.statement_total
    }
}

/// One reference in the order the two sides are walked: its positions on each side, where it has
/// any. A reference absent from a side is what makes it pending or missing.
struct Group {
    reference: String,
    ledger_indexes: Option<Vec<usize>>,
    statement_indexes: Option<Vec<usize>>,
}

/// The first place the two sides disagree, with both running balances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Zero-based position of the reference group in the comparison.
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
    /// References present on both sides and adding up to the same total.
    pub matched: usize,
    /// Statement references the ledger has never seen.
    pub missing_from_ledger: Vec<String>,
    /// Ledger references the statement does not have.
    pub pending_in_ledger: Vec<String>,
    /// Every reference either side carries, and what it consumed.
    pub settlements: Vec<Settlement>,
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

/// Groups the positions of a sequence by reference, in first-appearance order.
///
/// `n` is one statement's line count, so the linear scan through the groups is deliberate: a map
/// would be a dependency or a hash of a sequence that fits in a cache line.
fn group_by_reference(entries: &[(String, Amount)]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, (reference, _)) in entries.iter().enumerate() {
        match groups.iter_mut().find(|(name, _)| name == reference) {
            Some((_, indexes)) => indexes.push(index),
            None => groups.push((reference.clone(), vec![index])),
        }
    }
    groups
}

/// The sum of the entries at `indexes`.
fn total_of(indexes: &[usize], entries: &[(String, Amount)]) -> Amount {
    indexes.iter().fold(Amount::ZERO, |total, index| {
        total.checked_add(entries[*index].1).unwrap_or(total)
    })
}

/// "1 entry" or "2 entries", for a message a human reads.
fn entries(count: usize) -> String {
    if count == 1 {
        "1 entry".to_owned()
    } else {
        format!("{count} entries")
    }
}

/// Compares the ledger with a statement for one account.
///
/// The ledger's referenced postings on the account are matched with the statement's entries **by
/// reference**: everything either side carries under one reference is compared as a total, which is
/// what makes a split (one ledger entry, several statement entries) and a merge (the other way
/// round) reconcile when they add up.
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

    let statements: Vec<(String, Amount)> = statement
        .iter()
        .map(|entry| (entry.reference.clone(), entry.amount))
        .collect();

    let ledger_refs: BTreeSet<&str> = side
        .iter()
        .map(|(reference, _)| reference.as_str())
        .collect();
    let statement_refs: BTreeSet<&str> = statements
        .iter()
        .map(|(reference, _)| reference.as_str())
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

    // the matching step: the ledger's references in order, then the ones only the statement has
    let ledger_groups = group_by_reference(&side);
    let statement_groups = group_by_reference(&statements);
    let mut order: Vec<Group> = ledger_groups
        .iter()
        .map(|(reference, ledger_indexes)| {
            let statement_indexes = statement_groups
                .iter()
                .find(|(name, _)| name == reference)
                .map(|(_, indexes)| indexes.clone());
            Group {
                reference: reference.clone(),
                ledger_indexes: Some(ledger_indexes.clone()),
                statement_indexes,
            }
        })
        .collect();
    for (reference, indexes) in &statement_groups {
        if !ledger_refs.contains(reference.as_str()) {
            order.push(Group {
                reference: reference.clone(),
                ledger_indexes: None,
                statement_indexes: Some(indexes.clone()),
            });
        }
    }

    let mut ledger_running = Amount::ZERO;
    let mut statement_running = Amount::ZERO;
    let mut matched = 0_usize;
    let mut first_divergence: Option<Divergence> = None;
    let mut settlements: Vec<Settlement> = Vec::new();

    for (index, group) in order.iter().enumerate() {
        let Group {
            reference,
            ledger_indexes,
            statement_indexes,
        } = group;
        let ledger_total = ledger_indexes
            .as_deref()
            .map_or(Amount::ZERO, |indexes| total_of(indexes, &side));
        let statement_total = statement_indexes
            .as_deref()
            .map_or(Amount::ZERO, |indexes| total_of(indexes, &statements));
        let settlement = Settlement {
            reference: reference.clone(),
            ledger_indexes: ledger_indexes.clone().unwrap_or_default(),
            statement_indexes: statement_indexes.clone().unwrap_or_default(),
            ledger_total,
            statement_total,
        };

        if settlement.is_settled() {
            matched += 1;
            ledger_running = ledger_running
                .checked_add(ledger_total)
                .unwrap_or(ledger_running);
            statement_running = statement_running
                .checked_add(statement_total)
                .unwrap_or(statement_running);
            settlements.push(settlement);
            continue;
        }

        if first_divergence.is_none() {
            let reason = match (ledger_indexes, statement_indexes) {
                (Some(_), Some(_)) => ReconciliationReason::AmountMismatch,
                (Some(_), None) => ReconciliationReason::PendingInLedger,
                _ => ReconciliationReason::MissingFromLedger,
            };
            let detail = match (ledger_indexes, statement_indexes) {
                (Some(ledger_indexes), Some(statement_indexes)) => format!(
                    "{reference}: the ledger booked {ledger_total} across {}, the statement says {statement_total} across {}",
                    entries(ledger_indexes.len()),
                    entries(statement_indexes.len())
                ),
                (Some(_), None) => {
                    format!("{reference}: in the ledger ({ledger_total}), not in the statement")
                }
                _ => format!(
                    "{reference}: in the statement ({statement_total}), not in the ledger"
                ),
            };
            first_divergence = Some(Divergence {
                index,
                reason,
                ledger_reference: ledger_indexes.as_ref().map(|_| reference.clone()),
                statement_reference: statement_indexes.as_ref().map(|_| reference.clone()),
                ledger_running,
                statement_running,
                difference: statement_running
                    .checked_add(ledger_running.negated())
                    .unwrap_or(Amount::ZERO),
                detail,
            });
        }
        settlements.push(settlement);
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
        settlements,
        first_divergence,
        ledger_final,
        statement_final,
    }
}
