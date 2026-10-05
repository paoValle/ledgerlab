//! Invariants, checked independently of the code that maintains them.
//!
//! `verify` does not trust the balances the ledger carries around: it recomputes them from the
//! accepted transactions and compares. That is the whole point — a ledger whose fast path and
//! whose slow path disagree is a ledger with a bug, and the only way to find out is to have two
//! paths that must agree.
//!
//! Every check here is one a customer could ask about: does every transaction balance, does the
//! whole ledger sum to zero, does every account respect its declared minimum, do the balances
//! match the transactions.

use std::collections::BTreeMap;

use crate::ledger::Ledger;
use crate::money::Amount;

/// Something that should never be true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Which check failed.
    pub kind: ViolationKind,
    /// The evidence.
    pub detail: String,
}

/// The checks, one variant each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// A transaction in the log does not sum to zero.
    UnbalancedTransaction,
    /// The sum of every balance is not zero.
    GlobalSumNotZero,
    /// A recomputed balance differs from the one the ledger reports.
    BalanceMismatch,
    /// An account is below the minimum it declares.
    BelowMinimum,
    /// A posting names an account that is not in the chart.
    UnknownAccount,
}

/// The result of an audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audit {
    /// Accounts in the chart.
    pub accounts: usize,
    /// Transactions accepted.
    pub transactions: usize,
    /// The sum of every balance: zero, or something is very wrong.
    pub global_sum: Amount,
    /// Whether the recomputed balances matched the ledger's.
    pub recomputed_matches: bool,
    /// Everything that failed.
    pub violations: Vec<Violation>,
}

impl Audit {
    /// Whether the ledger is internally consistent.
    #[must_use]
    pub fn passes(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Audits a ledger.
#[must_use]
pub fn audit(ledger: &Ledger) -> Audit {
    let mut violations = Vec::new();

    // 1. every transaction balances, and touches only known accounts
    for (sequence, transaction) in ledger.entries() {
        match transaction.sum() {
            Ok(sum) if sum.is_zero() => {}
            Ok(sum) => violations.push(Violation {
                kind: ViolationKind::UnbalancedTransaction,
                detail: format!("sequence {sequence} ({}) sums to {sum}", transaction.id),
            }),
            Err(error) => violations.push(Violation {
                kind: ViolationKind::UnbalancedTransaction,
                detail: format!(
                    "sequence {sequence} ({}) failed to sum: {error}",
                    transaction.id
                ),
            }),
        }
        for posting in &transaction.postings {
            if !ledger.accounts().contains_key(&posting.account) {
                violations.push(Violation {
                    kind: ViolationKind::UnknownAccount,
                    detail: format!("sequence {sequence} posts to {:?}", posting.account),
                });
            }
        }
    }

    // 2. the balances the ledger reports must equal the balances its own transactions imply
    let mut recomputed: BTreeMap<String, Amount> = ledger
        .accounts()
        .keys()
        .map(|id| (id.clone(), Amount::ZERO))
        .collect();
    for (_, transaction) in ledger.entries() {
        for posting in &transaction.postings {
            let current = recomputed
                .get(&posting.account)
                .copied()
                .unwrap_or(Amount::ZERO);
            let next = current.checked_add(posting.amount).unwrap_or(current);
            recomputed.insert(posting.account.clone(), next);
        }
    }
    let recomputed_matches = ledger
        .balances()
        .iter()
        .all(|(id, balance)| recomputed.get(id).copied().unwrap_or(Amount::ZERO) == *balance);
    if !recomputed_matches {
        for (id, balance) in ledger.balances() {
            let recomputed = recomputed.get(id).copied().unwrap_or(Amount::ZERO);
            if recomputed != *balance {
                violations.push(Violation {
                    kind: ViolationKind::BalanceMismatch,
                    detail: format!(
                        "{id}: ledger says {balance}, its transactions say {recomputed}"
                    ),
                });
            }
        }
    }

    // 3. the sum of everything is zero, by construction: if it is not, the construction is broken
    let mut global_sum = Amount::ZERO;
    for balance in ledger.balances().values() {
        global_sum = global_sum.checked_add(*balance).unwrap_or(global_sum);
    }
    if !global_sum.is_zero() {
        violations.push(Violation {
            kind: ViolationKind::GlobalSumNotZero,
            detail: format!("every balance sums to {global_sum}"),
        });
    }

    // 4. every account respects the minimum it declares
    for (id, account) in ledger.accounts() {
        if let Some(minimum) = account.min_balance {
            let balance = ledger.balance(id);
            if balance < minimum {
                violations.push(Violation {
                    kind: ViolationKind::BelowMinimum,
                    detail: format!("{id} holds {balance}, below its minimum {minimum}"),
                });
            }
        }
    }

    Audit {
        accounts: ledger.accounts().len(),
        transactions: ledger.len(),
        global_sum,
        recomputed_matches,
        violations,
    }
}
