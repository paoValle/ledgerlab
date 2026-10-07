# ledgerlab

> A double-entry ledger that **proves its invariants** and says **where** it stopped agreeing with
> somebody else's statement.

Every tool can report a difference. Almost none reports where it began:

```console
$ ledgerlab reconcile --log ledger.jsonl --account assets:bank --statement bank.jsonl   # exit 1
Reconciliation of `assets:bank`
| measure | value |
|---|---|
| entries in the ledger | 4 |
| entries in the statement | 4 |
| references matched | 3 |
| final balance, ledger | 128.950000 |
| final balance, statement | 128.500000 |
| difference | -0.450000 |

**First divergence at entry 3** (amount mismatch): fee_1: the ledger booked -1.050000, the statement says -1.500000

The running balances before it: ledger 130.000000, statement 130.000000 (difference 0.000000).
```

"The bank is 0.45 short" is a puzzle. "Entry 3: the ledger booked 1.05 in fees, the bank charged
1.50, and everything before it agreed to the cent" is an answer.

## The problem

Every product that charges money has a ledger, and most of them are subtly wrong in the same four
ways: floats, no idempotency on retries, no reconciliation with the payment processor, and no
audit trail. The bill arrives as a question nobody can answer — *"the sum of our orders does not
match the PSP balance and nobody knows since when"* — and it is unanswerable exactly because the
ledger stored conclusions instead of events.

## What it does

- **Exact money**: `i64` micro-units, no floats anywhere. Parsing **refuses** what it cannot
  represent (`0.0000001` is an error, not `0.00`), because rounding a cent away is the ledger lying.
- **Double entry as an invariant**: every transaction sums to zero, and a transaction that would
  leave an account below the minimum it declares is refused *entirely*.
- **Idempotency**: applying the same file twice charges nobody twice.
- **Append-only, durable log**: JSONL, flushed with `sync_all` on every append, with no `update`
  and no `delete`. Corrections are transactions, so history stays readable.
- **Balances derived, not authoritative**: `Ledger::replay` rebuilds state from the log, and the
  audit recomputes the balances from the transactions and compares — two paths that must agree.
- **A snapshot a replay can start from**: `snapshot` appends the whole state as one event, and a
  replay that finds one adopts it instead of re-applying everything before it.
- **Compaction that cannot lose the history**: `compact` writes a **new** file with the newest
  snapshot and the events after it, and refuses to overwrite a log. What it drops is what the
  snapshot already covers; the accepted transactions stay inside the snapshot, because idempotency is
  a promise about them and a log that cannot answer "was this one of them" charges twice.
- **Reconciliation with a reason**: each divergence is classified as an amount mismatch, an entry
  the ledger never saw, a pending entry, or an ordering problem, because collapsing those into one
  "delta" is how a timing difference gets escalated and a real one gets ignored.
- **Exit codes a CI can gate on**: `0` agrees, `1` a divergence that means the ledger is wrong,
  `2` a usage or input error. A pending entry is reported and returns `0`.

```console
$ ledgerlab verify --log ledger.jsonl
4 account(s), 4 transaction(s), sum of everything 0.000000, recomputed balances match: yes
$ ledgerlab snapshot --log ledger.jsonl
snapshot at sequence 4: 4 account(s), 4 transaction(s)
$ ledgerlab compact --log ledger.jsonl --out ledger-compact.jsonl
wrote ledger-compact.jsonl: 1 event(s) kept (the newest snapshot and the tail), 8 dropped
$ ledgerlab verify --log ledger-compact.jsonl
4 account(s), 4 transaction(s), sum of everything 0.000000, recomputed balances match: yes
$ ledgerlab apply --log ledger-compact.jsonl --pending activity.jsonl     # the promise survives
0 transaction(s) applied, 4 already applied (idempotent), 4 in the log
```

```console
$ ledgerlab apply --log ledger.jsonl --pending activity.jsonl
4 transaction(s) applied, 0 already applied (idempotent), 4 in the log
$ ledgerlab apply --log ledger.jsonl --pending activity.jsonl     # the same file again
0 transaction(s) applied, 4 already applied (idempotent), 4 in the log
$ ledgerlab verify --log ledger.jsonl
4 account(s), 4 transaction(s), sum of everything 0.000000, recomputed balances match: yes
```

## How it works

```
chart.jsonl ──► open ──┐
                       ├──► ledger.jsonl (append-only, fsync, the only truth)
activity.jsonl ─► apply┘         │
                                 ├──► replay ──► balances (a cache, rebuilt on every command)
                                 ├──► audit    ──► invariants, by recomputation
bank.jsonl ──────────────────────┴──► reconcile ──► first divergence, with its reason
```

```text
src/
  money.rs      exact signed micro-units, strict parsing, no floats
  ledger.rs     accounts, transactions, double entry, idempotency, replay
  log.rs        append-only JSONL, fsync, torn-write detection
  audit.rs      the invariants, checked independently of the code that maintains them
  reconcile.rs  comparison with a statement, and the first entry where they disagree
  main.rs       the six commands, and the exit codes
```

Input files (charts, transactions, statements) are JSONL and may contain `#` comments: a fixture
that explains itself is worth more than one that cannot. The **event log** is stricter — it is
written by this program, so a line that does not parse there is damage, not a comment. A torn write
is reported with its line number and never skipped: the transaction it was recording may already
have been applied.

## The invariants are tested by generation, not only by example

`cargo test` runs **13 tests**: five properties over generated books (`proptest`), five
concrete cases, and three unit tests for the money type itself.

| property | what it means |
|---|---|
| any generated book audits clean | whatever the generator produces, balances match the transactions and the sum is zero |
| a book can be applied twice | the retry after a timeout returns `AlreadyApplied` and changes nothing |
| replay rebuilds the same ledger | the log alone is enough: same balances, same sequence, audit clean |
| a statement taken from the ledger always reconciles | the tool does not invent work |
| a changed amount diverges exactly there | flip one amount at index *k*, and the report points at *k* with `AmountMismatch` |

The concrete cases are the ones a customer would ask about: an unbalanced transaction is refused
and **nothing moves**; the same idempotency key with different content is a conflict, not a retry; an account
below its declared minimum is refused; `0.1 + 0.2` is exactly `0.3` here, with the float version
asserted to be inexact as the reason this type exists; and a torn write is detected by line number.

```bash
make ci     # cargo fmt --check && cargo clippy -D warnings && cargo test
```

## Usage

```bash
make setup          # cargo fetch
make demo           # build the demo ledger from examples/, twice (idempotency included)
make verify         # audit it
make reconcile      # against the diverging statement: exits 1
make report         # writes reports/latest.md (the artifact committed here)
```

```
ledgerlab open      --log <file> --accounts <chart.jsonl>
ledgerlab apply     --log <file> --pending <transactions.jsonl>
ledgerlab balances  --log <file> [--json]
ledgerlab verify    --log <file> [--json]
ledgerlab reconcile --log <file> --account <id> --statement <statement.jsonl> [--json]
ledgerlab report    --log <file> --account <id> --statement <label=file>... [--out reports/latest.md]
```

The fixtures tell one small story: a SaaS whose bank account receives two subscriptions, refunds
one, and pays a processor fee. Four transactions, four referenced entries, `assets:bank` ending at
`128.950000` — and three statements: one that agrees, one where the bank charged 1.50 in fees while
the ledger booked 1.05, and one where the refund has not cleared yet. The committed
[`reports/latest.md`](reports/latest.md) is the output of exactly that run.

## Scope, declared

Not here, and not claimed:

- **one currency.** Multi-currency needs rates with their own history (a rate is a fact with a
  timestamp, not a number), and doing it badly would be worse than not doing it.
- **not an accounting standard.** This is double entry with declared invariants, not GAAP or IFRS:
  no periods, no closing entries, no tax logic.
- **the statement comparison is positional.** Both sequences are assumed to be in chronological
  order, so a missing entry shifts the alignment — which is exactly why the report names the reason
  instead of printing a number.
- **one writer.** The log is append-only and durable, but nothing prevents two processes from
  interleaving appends. A single writer is a requirement of this version, not a detail.
- **no bank or PSP integration, no UI.** Statement files are files.
- **compaction does not make the log stop growing.** The snapshot carries the accepted
  transactions, because idempotency is a promise about them; what compaction buys is a shorter file
  to read and a replay that starts from a state instead of from the first event ever written. Real
  rotation (keeping a window of history and archiving the rest) is a different tool, and it would
  have to say where the archived part went.

## What I would do differently

- Idempotency keys are matched on content, which catches a key reused with different money, but not
  a client that retries with a *semantically* different transaction under the same key. That is a
  protocol problem (what does a retry mean?), and it needs a written answer, not a smarter compare.
- `reconcile` matches references one-to-one. Real statements have splits and merges (one charge
  settled in two payouts, one payout for ten charges), and those need a matching step before the
  comparison, not inside it.
- Compaction exists now (`snapshot` plus `compact`), and rotation does not: the log still holds every
  accepted transaction inside the snapshot, and a ledger with millions of events wants the archived
  part kept somewhere with a pointer to it.

## Development

```bash
git clone git@github.com:paoValle/ledgerlab.git
cd ledgerlab
make setup && make ci
```

## License

MIT © Paolo Valletta
