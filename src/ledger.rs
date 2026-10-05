//! The ledger: accounts, transactions, and the rules that make it a ledger.
//!
//! Double entry is not a format, it is an invariant: **every transaction sums to zero**. A
//! posting is signed (positive debits, negative credits) and a transaction that does not sum to
//! zero is rejected before it touches anything — including a transaction that would leave an
//! account below its declared minimum.
//!
//! Two properties are what turn a list of transactions into something you can bill against:
//!
//! - **idempotency**: applying the same transaction twice is a no-op, not a double charge. A
//!   retry after a timeout is the normal case, not the exception, and a ledger that cannot
//!   survive one is a ledger with duplicate money in it;
//! - **replayability**: the state is derived from the event log, never authoritative in memory.
//!   `Ledger::replay` rebuilds balances from events alone, and the audit compares the two.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::money::Amount;

/// What kind of account it is, for reporting. It does not change the arithmetic: signs are
/// explicit in the postings, and the one thing a kind is used for is grouping in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    /// Cash, bank, receivables.
    Asset,
    /// What is owed to others.
    Liability,
    /// Owner's stake.
    Equity,
    /// Money earned.
    Revenue,
    /// Money spent.
    Expense,
}

/// An account, with the invariant it must respect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Unique identifier, e.g. `assets:bank`.
    pub id: String,
    /// What kind of account it is.
    pub kind: AccountKind,
    /// The lowest balance this account may ever have. `None` means no constraint.
    ///
    /// It is written as an explicit number instead of a "normal balance" convention because the
    /// convention is where bugs hide: a bank account that may not go negative says
    /// `min_balance = "0.000000"`, and a credit card says `min_balance = null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_balance: Option<Amount>,
}

/// One side of a transaction. Positive debits, negative credits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    /// The account this side touches.
    pub account: String,
    /// The signed amount.
    pub amount: Amount,
}

/// A transaction: the unit that must balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    /// Identifier, unique in the ledger.
    pub id: String,
    /// The key that makes a retry a no-op. Often the same as `id`, deliberately settable apart.
    pub idempotency_key: String,
    /// The external reference (a PSP charge, an invoice), used by reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Human note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
    /// The two or more sides.
    pub postings: Vec<Posting>,
}

impl Transaction {
    /// The sum of its postings. Zero means it balances.
    pub fn sum(&self) -> Result<Amount, LedgerError> {
        let mut total = Amount::ZERO;
        for posting in &self.postings {
            total = total
                .checked_add(posting.amount)
                .map_err(LedgerError::Money)?;
        }
        Ok(total)
    }
}

/// What the log stores: the only truth there is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An account became part of the chart.
    AccountOpened {
        /// The account.
        account: Account,
    },
    /// A transaction was accepted, with the sequence number it was accepted at.
    TransactionApplied {
        /// Position in the log, starting at 1.
        sequence: u64,
        /// The transaction, as accepted.
        transaction: Transaction,
    },
}

/// The result of applying a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// It was accepted, at this sequence number.
    Recorded(u64),
    /// The same key with the same content was already applied: nothing changed.
    AlreadyApplied,
}

/// What can go wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    /// A transaction needs at least two sides.
    TooFewPostings {
        /// How many it had.
        got: usize,
    },
    /// The postings do not sum to zero.
    Unbalanced {
        /// The sum it had.
        sum: Amount,
    },
    /// A posting names an account that was never opened.
    UnknownAccount {
        /// The account.
        id: String,
    },
    /// The same idempotency key arrived with different content.
    IdempotencyConflict {
        /// The key.
        key: String,
    },
    /// The same transaction id arrived twice.
    DuplicateId {
        /// The id.
        id: String,
    },
    /// The log's sequence numbers do not match the order they appear in.
    SequenceMismatch {
        /// What the position required.
        expected: u64,
        /// What the log said.
        got: u64,
    },
    /// The account was opened twice with different definitions.
    AccountConflict {
        /// The account.
        id: String,
    },
    /// The transaction would leave an account below its minimum.
    BelowMinimum {
        /// The account.
        account: String,
        /// The balance it would have had.
        balance: Amount,
        /// The minimum it declares.
        minimum: Amount,
    },
    /// Arithmetic on money failed.
    Money(crate::money::MoneyError),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewPostings { got } => write!(
                f,
                "a transaction needs at least two postings, this one has {got}: double entry is the invariant, not a style"
            ),
            Self::Unbalanced { sum } => write!(
                f,
                "the postings sum to {sum}, not zero: an unbalanced transaction is not a transaction"
            ),
            Self::UnknownAccount { id } => write!(f, "account {id:?} was never opened"),
            Self::IdempotencyConflict { key } => write!(
                f,
                "idempotency key {key:?} was already used for a different transaction: refusing it is the only safe answer"
            ),
            Self::DuplicateId { id } => write!(f, "transaction id {id:?} is already in the ledger"),
            Self::SequenceMismatch { expected, got } => write!(
                f,
                "the log says sequence {got} where {expected} was expected: a log whose order does not match its own numbers cannot be replayed"
            ),
            Self::AccountConflict { id } => {
                write!(f, "account {id:?} is already open with a different definition")
            }
            Self::BelowMinimum {
                account,
                balance,
                minimum,
            } => write!(
                f,
                "account {account:?} would hold {balance}, below its declared minimum {minimum}"
            ),
            Self::Money(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LedgerError {}

/// The ledger: derived state, rebuilt from events.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    accounts: BTreeMap<String, Account>,
    balances: BTreeMap<String, Amount>,
    applied: BTreeMap<String, Transaction>,
    entries: Vec<(u64, Transaction)>,
    sequence: u64,
}

impl Ledger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an account to the chart.
    ///
    /// Opening the same account twice with the same definition is a no-op, so replaying a chart
    /// file is safe; opening it with a different definition is an error, because silently
    /// changing an invariant is how a minimum balance stops being enforced.
    pub fn open(&mut self, account: Account) -> Result<bool, LedgerError> {
        match self.accounts.get(&account.id) {
            Some(existing) if *existing == account => Ok(false),
            Some(_) => Err(LedgerError::AccountConflict { id: account.id }),
            None => {
                self.balances.insert(account.id.clone(), Amount::ZERO);
                self.accounts.insert(account.id.clone(), account);
                Ok(true)
            }
        }
    }

    /// Applies a transaction, or refuses it without changing anything.
    pub fn apply(&mut self, transaction: Transaction) -> Result<Applied, LedgerError> {
        // 1. a retry first: the same key with the same content is the normal case, and it must
        //    succeed even if other transactions have since moved the balances
        if let Some(previous) = self.applied.get(&transaction.idempotency_key) {
            return if same_content(previous, &transaction) {
                Ok(Applied::AlreadyApplied)
            } else {
                Err(LedgerError::IdempotencyConflict {
                    key: transaction.idempotency_key,
                })
            };
        }

        // 2. shape
        if transaction.postings.len() < 2 {
            return Err(LedgerError::TooFewPostings {
                got: transaction.postings.len(),
            });
        }
        let sum = transaction.sum()?;
        if !sum.is_zero() {
            return Err(LedgerError::Unbalanced { sum });
        }
        for posting in &transaction.postings {
            if !self.accounts.contains_key(&posting.account) {
                return Err(LedgerError::UnknownAccount {
                    id: posting.account.clone(),
                });
            }
        }
        if self
            .entries
            .iter()
            .any(|(_, entry)| entry.id == transaction.id)
        {
            return Err(LedgerError::DuplicateId { id: transaction.id });
        }

        // 3. the invariants declared by the accounts, checked on a copy: a transaction either
        //    applies entirely or not at all
        let mut next = self.balances.clone();
        for posting in &transaction.postings {
            let current = next.get(&posting.account).copied().unwrap_or(Amount::ZERO);
            next.insert(
                posting.account.clone(),
                current
                    .checked_add(posting.amount)
                    .map_err(LedgerError::Money)?,
            );
        }
        for (id, balance) in &next {
            if let Some(minimum) = self
                .accounts
                .get(id)
                .and_then(|account| account.min_balance)
            {
                if *balance < minimum {
                    return Err(LedgerError::BelowMinimum {
                        account: id.clone(),
                        balance: *balance,
                        minimum,
                    });
                }
            }
        }

        // 4. accepted: commit, and remember the key
        self.balances = next;
        self.sequence += 1;
        self.entries.push((self.sequence, transaction.clone()));
        self.applied
            .insert(transaction.idempotency_key.clone(), transaction);
        Ok(Applied::Recorded(self.sequence))
    }

    /// Rebuilds a ledger from events. This is what makes the log the truth and the state a cache.
    pub fn replay(events: &[Event]) -> Result<Self, LedgerError> {
        let mut ledger = Self::new();
        for event in events {
            match event {
                Event::AccountOpened { account } => {
                    ledger.open(account.clone())?;
                }
                Event::TransactionApplied {
                    sequence,
                    transaction,
                } => {
                    ledger.apply(transaction.clone())?;
                    // `apply` assigned the next number; the log must agree with it, otherwise the
                    // file has been reordered or forged and replay would silently invent a history
                    if *sequence != ledger.sequence {
                        return Err(LedgerError::SequenceMismatch {
                            expected: ledger.sequence,
                            got: *sequence,
                        });
                    }
                }
            }
        }
        Ok(ledger)
    }

    /// The accounts.
    #[must_use]
    pub fn accounts(&self) -> &BTreeMap<String, Account> {
        &self.accounts
    }

    /// The derived balances.
    #[must_use]
    pub fn balances(&self) -> &BTreeMap<String, Amount> {
        &self.balances
    }

    /// The balance of one account.
    #[must_use]
    pub fn balance(&self, account: &str) -> Amount {
        self.balances.get(account).copied().unwrap_or(Amount::ZERO)
    }

    /// The accepted transactions, in sequence order.
    #[must_use]
    pub fn entries(&self) -> &[(u64, Transaction)] {
        &self.entries
    }

    /// How many transactions were accepted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was accepted yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The last sequence number assigned.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Whether two transactions are the same request, ignoring nothing that carries meaning.
///
/// `id` is excluded on purpose: a retry may arrive with a freshly generated id, and refusing it
/// because of that would turn a safe retry into a failure.
fn same_content(a: &Transaction, b: &Transaction) -> bool {
    a.idempotency_key == b.idempotency_key
        && a.reference == b.reference
        && a.memo == b.memo
        && a.postings == b.postings
}
