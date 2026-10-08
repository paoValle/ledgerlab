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

/// A charge settled in two payouts reconciles: a reference is matched by total, not by position.
#[test]
fn a_charge_settled_in_two_payouts_reconciles() {
    let mut ledger = ledger();
    apply_one(
        &mut ledger,
        "ch_1",
        "revenue:subscriptions",
        Amount::from_micros(100_000_000),
    );

    let statement = vec![
        StatementEntry {
            reference: "ch_1".to_owned(),
            amount: Amount::from_micros(60_000_000),
        },
        StatementEntry {
            reference: "ch_1".to_owned(),
            amount: Amount::from_micros(40_000_000),
        },
    ];

    let result = reconcile(&ledger, "assets:bank", &statement);

    assert!(result.is_clean(), "{:?}", result.first_divergence);
    assert_eq!(result.matched, 1);
    assert_eq!(result.ledger_entries, 1);
    assert_eq!(result.statement_entries, 2);
    let settlement = &result.settlements[0];
    assert_eq!(settlement.ledger_indexes, vec![0]);
    assert_eq!(settlement.statement_indexes, vec![0, 1]);
    assert!(settlement.is_settled());
    assert!(result.final_difference().is_zero());
}

/// A payout covering two charges reconciles the other way round.
#[test]
fn a_payout_covering_two_charges_reconciles() {
    let mut ledger = ledger();
    apply_one(
        &mut ledger,
        "po_1",
        "revenue:subscriptions",
        Amount::from_micros(60_000_000),
    );
    apply_one(
        &mut ledger,
        "po_1",
        "revenue:subscriptions",
        Amount::from_micros(40_000_000),
    );

    let statement = vec![StatementEntry {
        reference: "po_1".to_owned(),
        amount: Amount::from_micros(100_000_000),
    }];

    let result = reconcile(&ledger, "assets:bank", &statement);

    assert!(result.is_clean(), "{:?}", result.first_divergence);
    assert_eq!(result.matched, 1);
    assert_eq!(result.ledger_entries, 2);
    assert_eq!(result.statement_entries, 1);
    assert_eq!(result.settlements[0].ledger_indexes, vec![0, 1]);
    assert_eq!(result.settlements[0].statement_indexes, vec![0]);
}

/// Several entries under one reference that do not add up are an amount mismatch, not a shape.
#[test]
fn a_split_that_does_not_add_up_is_an_amount_mismatch() {
    let mut ledger = ledger();
    apply_one(
        &mut ledger,
        "ch_2",
        "revenue:subscriptions",
        Amount::from_micros(100_000_000),
    );

    let statement = vec![
        StatementEntry {
            reference: "ch_2".to_owned(),
            amount: Amount::from_micros(60_000_000),
        },
        StatementEntry {
            reference: "ch_2".to_owned(),
            amount: Amount::from_micros(30_000_000),
        },
    ];

    let result = reconcile(&ledger, "assets:bank", &statement);

    let divergence = result
        .first_divergence
        .expect("90.00 does not add up to 100.00");
    assert_eq!(
        divergence.reason,
        ledgerlab::ReconciliationReason::AmountMismatch
    );
    assert_eq!(divergence.ledger_reference.as_deref(), Some("ch_2"));
    assert!(
        divergence.detail.contains("across 2 entries"),
        "{}",
        divergence.detail
    );
}

/// Applies one bank inflow of `amount` under `reference` against `counterpart`.
fn apply_one(ledger: &mut Ledger, reference: &str, counterpart: &str, amount: Amount) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    ledger
        .apply(Transaction {
            id: format!("tx-{reference}-{sequence}"),
            idempotency_key: format!("key-{reference}-{sequence}"),
            reference: Some(reference.to_owned()),
            memo: None,
            postings: vec![
                Posting {
                    account: "assets:bank".to_owned(),
                    amount,
                },
                Posting {
                    account: counterpart.to_owned(),
                    amount: amount.negated(),
                },
            ],
        })
        .expect("the chart allows an inflow on the bank");
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

#[test]
fn a_compacted_log_replays_to_the_same_state_as_the_whole_one() {
    // what compaction has to preserve, and the reason the whole thing is allowed at all: dropping
    // the events a snapshot covers must not change a single number
    let mut events: Vec<Event> = chart()
        .into_iter()
        .map(|account| Event::AccountOpened { account })
        .collect();
    let mut ledger = ledger();
    for transaction in books(&[(0, 1, 1_000_000), (2, 3, 5_000)]) {
        ledger.apply(transaction.clone()).expect("applies");
        events.push(Event::TransactionApplied {
            sequence: ledger.sequence(),
            transaction,
        });
    }

    let snapshot = ledger.snapshot();
    let full = &events;
    let compacted = ledgerlab::compact(&[full.clone(), vec![snapshot.clone()]].concat())
        .expect("a snapshot to compact from");

    let whole = Ledger::replay(full).expect("replays");
    let from_snapshot = Ledger::replay(&compacted).expect("replays from the snapshot");

    assert_eq!(from_snapshot.accounts(), whole.accounts());
    assert_eq!(from_snapshot.balances(), whole.balances());
    assert_eq!(from_snapshot.entries(), whole.entries());
    assert_eq!(from_snapshot.sequence(), whole.sequence());
    assert_eq!(compacted.len(), 1, "everything before the snapshot is gone");

    // and the tail is still applied on top of it
    let mut extra = books(&[(0, 1, 700)]).remove(0);
    extra.id = "after-the-snapshot".to_owned();
    extra.idempotency_key = "after-the-snapshot".to_owned();
    let tail = Event::TransactionApplied {
        sequence: ledger.sequence() + 1,
        transaction: extra,
    };
    let with_tail = Ledger::replay(&[compacted, vec![tail.clone()]].concat()).expect("replays");
    let with_tail_whole = Ledger::replay(&[events, vec![tail]].concat()).expect("replays");
    assert_eq!(with_tail.balances(), with_tail_whole.balances());
    assert_eq!(with_tail.sequence(), with_tail_whole.sequence());
}

#[test]
fn idempotency_survives_a_compaction() {
    // the promise a snapshot must not break: applying the same file twice charges nobody twice, and
    // it has to keep holding after the transactions are no longer in the log as events
    let mut events: Vec<Event> = chart()
        .into_iter()
        .map(|account| Event::AccountOpened { account })
        .collect();
    let mut ledger = ledger();
    let transaction = books(&[(0, 1, 1_000_000)]).remove(0);
    ledger.apply(transaction.clone()).expect("applies");
    events.push(Event::TransactionApplied {
        sequence: ledger.sequence(),
        transaction: transaction.clone(),
    });
    let snapshot = ledger.snapshot();
    let compacted = ledgerlab::compact(&[events, vec![snapshot]].concat()).expect("a snapshot");

    let mut replayed = Ledger::replay(&compacted).expect("replays");
    assert_eq!(
        replayed.apply(transaction.clone()).expect("a retry"),
        Applied::AlreadyApplied
    );

    let mut impostor = transaction;
    impostor.postings[0].amount = Amount::from_micros(999_999);
    impostor.postings[1].amount = Amount::from_micros(-999_999);
    assert!(matches!(
        replayed.apply(impostor),
        Err(LedgerError::IdempotencyConflict { .. })
    ));
}

#[test]
fn a_snapshot_that_does_not_add_up_is_refused_instead_of_believed() {
    let mut events: Vec<Event> = chart()
        .into_iter()
        .map(|account| Event::AccountOpened { account })
        .collect();
    let mut ledger = ledger();
    for transaction in books(&[(0, 1, 1_000_000), (2, 3, 5_000)]) {
        ledger.apply(transaction.clone()).expect("applies");
        events.push(Event::TransactionApplied {
            sequence: ledger.sequence(),
            transaction,
        });
    }
    let snapshot = ledger.snapshot();
    assert!(
        Ledger::replay(std::slice::from_ref(&snapshot)).is_ok(),
        "the honest one replays"
    );

    // a balance that is not the sum of the transactions it carries
    let Event::BalancesSnapshot {
        accounts,
        balances,
        entries,
        ..
    } = snapshot.clone()
    else {
        panic!("a snapshot");
    };
    let mut lying = balances.clone();
    lying.insert("assets:bank".to_owned(), Amount::from_micros(999));
    let forged = Event::BalancesSnapshot {
        sequence: ledger.sequence(),
        accounts: accounts.clone(),
        balances: lying,
        entries: entries.clone(),
    };
    let error = Ledger::replay(&[forged]).expect_err("a lie is not replayed");
    assert!(
        matches!(error, LedgerError::SnapshotInvalid { .. }),
        "{error:?}"
    );

    // a snapshot that covers less than the events before it: a rollback, not a summary
    let mut earlier = snapshot.clone();
    if let Event::BalancesSnapshot { sequence, .. } = &mut earlier {
        *sequence -= 1;
    }
    let error = Ledger::replay(&[snapshot.clone(), earlier]).expect_err("no going backwards");
    assert!(
        matches!(error, LedgerError::SnapshotInvalid { .. }),
        "{error:?}"
    );

    // and a snapshot whose transactions are numbered with a gap
    let mut holed = snapshot;
    if let Event::BalancesSnapshot { entries, .. } = &mut holed {
        entries.remove(0);
    }
    let error = Ledger::replay(&[holed]).expect_err("a gap is not a history");
    assert!(
        matches!(
            error,
            LedgerError::SequenceMismatch { .. } | LedgerError::SnapshotInvalid { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn compacting_without_a_snapshot_is_refused() {
    let events: Vec<Event> = chart()
        .into_iter()
        .map(|account| Event::AccountOpened { account })
        .collect();
    let error = ledgerlab::compact(&events).expect_err("nothing to compact from");
    assert!(
        matches!(error, ledgerlab::LogError::NothingToCompact),
        "{error:?}"
    );
}

#[test]
fn the_same_state_writes_the_same_snapshot_bytes() {
    // a snapshot that changed every time it was written could not be compared from one day to the
    // next, and comparing logs is what this tool is for
    let mut ledger = Ledger::new();
    for account in chart() {
        ledger.open(account).expect("opens");
    }
    for transaction in books(&[(0, 1, 1_000_000), (2, 3, 5_000)]) {
        ledger.apply(transaction).expect("applies");
    }
    let once = serde_json::to_string(&ledger.snapshot()).expect("encodes");
    let twice = serde_json::to_string(&ledger.snapshot()).expect("encodes");
    assert_eq!(once, twice);
}

#[test]
fn a_compacted_file_is_written_and_never_overwritten() {
    let source = temp_path("compact-source");
    let out = temp_path("compact-out");
    let log = Log::new(&source);
    let mut ledger = Ledger::new();
    for account in chart() {
        ledger.open(account.clone()).expect("opens");
        log.append(&Event::AccountOpened { account })
            .expect("append");
    }
    let transaction = books(&[(0, 1, 1_000_000)]).remove(0);
    ledger.apply(transaction.clone()).expect("applies");
    log.append(&Event::TransactionApplied {
        sequence: ledger.sequence(),
        transaction,
    })
    .expect("append");
    log.append(&ledger.snapshot()).expect("append");

    let events = log.read().expect("reads");
    let kept = ledgerlab::compact(&events).expect("a snapshot");
    assert_eq!(kept.len(), 1);
    Log::new(&out).write_new(&kept).expect("writes");

    // the input is untouched, and the output is not a file this command will replace
    assert_eq!(log.read().expect("reads").len(), events.len());
    let error = Log::new(&out)
        .write_new(&kept)
        .expect_err("never overwrites");
    assert!(
        matches!(error, ledgerlab::LogError::Exists { .. }),
        "{error:?}"
    );

    std::fs::remove_file(&source).ok();
    std::fs::remove_file(&out).ok();
}
