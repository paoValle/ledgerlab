//! The invariants, proved by generation and not only by example.
//!
//! Every test here answers a question a customer would ask: does every transaction balance, does
//! the ledger survive being applied twice, can it be rebuilt from its log, does a statement taken
//! from the ledger reconcile, and does it point at the right entry when one number changes.
//!
//! The last one is the product. A tool that reports "the difference is 0.45" is a calculator; a
//! tool that reports "entry 4, the ledger booked 1.05, the statement says 1.50" is an answer.

use std::sync::atomic::{AtomicUsize, Ordering};

use ledgerlab::{
    audit, reconcile, Account, AccountKind, Amount, Applied, Event, Ledger, LedgerError, Log,
    Posting, StatementEntry, Transaction,
};
use proptest::prelude::*;

const ACCOUNTS: [&str; 4] = [
    "assets:bank",
    "assets:receivable",
    "revenue:subscriptions",
    "expenses:fees",
];

fn chart() -> Vec<Account> {
    ACCOUNTS
        .iter()
        .map(|id| Account {
            id: (*id).to_owned(),
            kind: if id.starts_with("assets") {
                AccountKind::Asset
            } else if id.starts_with("revenue") {
                AccountKind::Revenue
            } else {
                AccountKind::Expense
            },
            // the bank may not go negative; everything else may
            min_balance: (*id == "assets:bank").then_some(Amount::ZERO),
        })
        .collect()
}

fn ledger() -> Ledger {
    let mut ledger = Ledger::new();
    for account in chart() {
        ledger.open(account).expect("a fresh chart opens");
    }
    ledger
}

/// Applies what the ledger accepts, and returns exactly the transactions that were recorded.
///
/// A generated book may legitimately be refused — the bank in this chart declares that it may not
/// go negative — and being refused is part of the contract, so the refusal is expected here and
/// anything else is not.
fn apply_all(ledger: &mut Ledger, transactions: &[Transaction]) -> Vec<Transaction> {
    let mut applied = Vec::new();
    for transaction in transactions {
        match ledger.apply(transaction.clone()) {
            Ok(Applied::Recorded(_)) => applied.push(transaction.clone()),
            // already applied, or refused for a reason the chart declares: both are the contract
            Ok(Applied::AlreadyApplied) | Err(LedgerError::BelowMinimum { .. }) => {}
            Err(other) => panic!("a generated book must be structurally valid: {other}"),
        }
    }
    applied
}

/// Builds books out of a generated list of (from, to, amount) triples.
///
/// The generator is what makes this a proof rather than an example: any triple produces a
/// balanced transaction, so the properties hold for thousands of books that were never written by
/// hand.
fn books(entries: &[(u8, u8, i64)]) -> Vec<Transaction> {
    entries
        .iter()
        .enumerate()
        .map(|(index, (from, to, amount))| {
            let from_index = usize::from(*from) % ACCOUNTS.len();
            let mut to_index = usize::from(*to) % ACCOUNTS.len();
            if to_index == from_index {
                to_index = (from_index + 1) % ACCOUNTS.len();
            }
            let from = ACCOUNTS[from_index];
            let to = ACCOUNTS[to_index];
            Transaction {
                id: format!("tx-{index}"),
                idempotency_key: format!("key-{index}"),
                reference: Some(format!("ref-{index}")),
                memo: None,
                postings: vec![
                    Posting {
                        account: from.to_owned(),
                        amount: Amount::from_micros(*amount),
                    },
                    Posting {
                        account: to.to_owned(),
                        amount: Amount::from_micros(-*amount),
                    },
                ],
            }
        })
        .collect()
}

fn book_strategy() -> impl Strategy<Value = Vec<(u8, u8, i64)>> {
    prop::collection::vec((0_u8..4, 0_u8..4, 1_i64..1_000_000), 1..12)
}

proptest! {
    /// Whatever the generator produces, the ledger stays consistent.
    #[test]
    fn any_book_audits_clean(entries in book_strategy()) {
        let mut ledger = ledger();
        let applied = apply_all(&mut ledger, &books(&entries));

        let report = audit(&ledger);
        prop_assert!(report.passes(), "{:?}", report.violations);
        prop_assert!(report.recomputed_matches);
        prop_assert!(report.global_sum.is_zero());
        prop_assert_eq!(report.transactions, applied.len());
    }

    /// Applying the same book twice is a no-op: the retry after a timeout must not double a charge.
    #[test]
    fn a_book_can_be_applied_twice(entries in book_strategy()) {
        let mut ledger = ledger();
        let applied = apply_all(&mut ledger, &books(&entries));
        let before = ledger.balances().clone();

        // the retry after a timeout: the same transactions, again
        for transaction in &applied {
            prop_assert_eq!(ledger.apply(transaction.clone()), Ok(Applied::AlreadyApplied));
        }
        prop_assert_eq!(ledger.balances(), &before);
        prop_assert_eq!(ledger.len(), applied.len());
    }

    /// The log is the truth: replaying it must rebuild exactly the same state.
    #[test]
    fn replay_rebuilds_the_same_ledger(entries in book_strategy()) {
        let mut ledger = ledger();
        apply_all(&mut ledger, &books(&entries));

        let mut events: Vec<Event> = chart()
            .into_iter()
            .map(|account| Event::AccountOpened { account })
            .collect();
        for (sequence, transaction) in ledger.entries() {
            events.push(Event::TransactionApplied {
                sequence: *sequence,
                transaction: transaction.clone(),
            });
        }

        let replayed = Ledger::replay(&events).expect("a log written by the ledger replays");
        prop_assert_eq!(replayed.balances(), ledger.balances());
        prop_assert_eq!(replayed.len(), ledger.len());
        prop_assert_eq!(replayed.sequence(), ledger.sequence());
        prop_assert!(audit(&replayed).passes());
    }

    /// A statement taken from the ledger always reconciles: the tool does not invent work.
    #[test]
    fn a_statement_from_the_ledger_always_reconciles(entries in book_strategy()) {
        let mut ledger = ledger();
        apply_all(&mut ledger, &books(&entries));

        let statement: Vec<StatementEntry> = ledger
            .entries()
            .iter()
            .flat_map(|(_, transaction)| {
                transaction
                    .postings
                    .iter()
                    .filter(|posting| posting.account == "assets:bank")
                    .filter_map(|posting| {
                        transaction.reference.clone().map(|reference| StatementEntry {
                            reference,
                            amount: posting.amount,
                        })
                    })
            })
            .collect();

        let result = reconcile(&ledger, "assets:bank", &statement);
        prop_assert!(result.is_clean(), "{:?}", result.first_divergence);
        prop_assert_eq!(result.matched, statement.len());
        prop_assert!(result.final_difference().is_zero());
    }

    /// Change one amount in that statement, and the report must point at exactly that entry.
    #[test]
    fn a_changed_amount_diverges_exactly_there(entries in book_strategy(), flip in any::<prop::sample::Index>()) {
        let mut ledger = ledger();
        apply_all(&mut ledger, &books(&entries));

        let mut statement: Vec<StatementEntry> = ledger
            .entries()
            .iter()
            .flat_map(|(_, transaction)| {
                transaction
                    .postings
                    .iter()
                    .filter(|posting| posting.account == "assets:bank")
                    .filter_map(|posting| {
                        transaction.reference.clone().map(|reference| StatementEntry {
                            reference,
                            amount: posting.amount,
                        })
                    })
            })
            .collect();
        prop_assume!(!statement.is_empty());

        let index = flip.index(statement.len());
        let entry = statement[index].clone();
        statement[index] = StatementEntry {
            reference: entry.reference.clone(),
            amount: entry.amount.checked_add(Amount::from_micros(1)).unwrap(),
        };

        let result = reconcile(&ledger, "assets:bank", &statement);
        let divergence = result.first_divergence.expect("one amount changed, so they diverge");
        prop_assert_eq!(divergence.index, index);
        prop_assert_eq!(divergence.reason, ledgerlab::ReconciliationReason::AmountMismatch);
        prop_assert_eq!(divergence.ledger_reference.as_deref(), Some(entry.reference.as_str()));
    }
}

#[test]
fn an_unbalanced_transaction_is_refused_and_nothing_moves() {
    let mut ledger = ledger();
    apply_all(&mut ledger, &books(&[(0, 1, 1_000_000), (2, 3, 500_000)]));
    let before = ledger.balances().clone();

    let broken = Transaction {
        id: "tx-broken".to_owned(),
        idempotency_key: "broken".to_owned(),
        reference: None,
        memo: None,
        postings: vec![
            Posting {
                account: "assets:bank".to_owned(),
                amount: Amount::from_micros(1_000_000),
            },
            Posting {
                account: "revenue:subscriptions".to_owned(),
                amount: Amount::from_micros(-999_999),
            },
        ],
    };

    let error = ledger
        .apply(broken)
        .expect_err("0.000001 of a difference is still unbalanced");
    assert!(
        matches!(error, ledgerlab::LedgerError::Unbalanced { .. }),
        "{error:?}"
    );
    assert_eq!(
        ledger.balances(),
        &before,
        "a refused transaction changes nothing"
    );
}

#[test]
fn an_account_below_its_declared_minimum_is_refused() {
    let mut ledger = ledger();
    let overdraft = Transaction {
        id: "tx-overdraft".to_owned(),
        idempotency_key: "overdraft".to_owned(),
        reference: None,
        memo: None,
        postings: vec![
            Posting {
                account: "assets:bank".to_owned(),
                amount: Amount::from_micros(-1),
            },
            Posting {
                account: "expenses:fees".to_owned(),
                amount: Amount::from_micros(1),
            },
        ],
    };

    let error = ledger
        .apply(overdraft)
        .expect_err("the bank may not go negative");
    assert!(
        matches!(error, ledgerlab::LedgerError::BelowMinimum { .. }),
        "{error:?}"
    );
}

#[test]
fn the_same_key_with_different_content_is_a_conflict_not_a_retry() {
    let mut ledger = ledger();
    let original = books(&[(0, 1, 1_000_000)]).remove(0);
    ledger.apply(original.clone()).expect("applies once");

    let mut impostor = original.clone();
    impostor.postings[0].amount = Amount::from_micros(999_999);
    impostor.postings[1].amount = Amount::from_micros(-999_999);

    let error = ledger
        .apply(impostor)
        .expect_err("same key, different money");
    assert!(
        matches!(error, ledgerlab::LedgerError::IdempotencyConflict { .. }),
        "{error:?}"
    );
}

#[test]
fn money_does_not_round_like_a_float() {
    // the classic: 0.1 + 0.2 is not 0.3 in binary floating point, and it is in micro-units
    let tenth: Amount = "0.100000".parse().expect("parses");
    let fifth: Amount = "0.200000".parse().expect("parses");
    let sum = tenth.checked_add(fifth).expect("no overflow");
    assert_eq!(sum, "0.300000".parse::<Amount>().expect("parses"));
    assert_eq!(sum.to_string(), "0.300000");

    // the same sum in binary floating point, parsed at runtime so nobody can fold it away
    let tenth: f64 = "0.1".parse().expect("parses");
    let fifth: f64 = "0.2".parse().expect("parses");
    assert!(
        (tenth + fifth - 0.3).abs() > 0.0,
        "if this ever fails, floats became exact and this module is unnecessary"
    );
}

#[test]
fn a_torn_write_is_detected_by_line_number_instead_of_being_skipped() {
    let path = temp_path("torn");
    let log = Log::new(&path);
    log.append(&Event::AccountOpened {
        account: chart().remove(0),
    })
    .expect("append");

    // a crash in the middle of a write: the last line is cut in half
    let mut contents = std::fs::read_to_string(&path).expect("read");
    contents.push_str("{\"type\":\"transaction_applied\",\"sequence\":1,\"transac");
    std::fs::write(&path, contents).expect("write");

    let error = log.read().expect_err("a half-written line is not a line");
    match error {
        ledgerlab::LogError::Line { line, .. } => {
            assert_eq!(line, 2, "the second line is the torn one");
        }
        other => panic!("expected a line error, got {other:?}"),
    }
    std::fs::remove_file(&path).ok();
}

fn temp_path(name: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "ledgerlab-{}-{name}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    path
}
