# ledgerlab — the ledger, audited and reconciled

Ledger: `target/ledgerlab-demo.jsonl` · 4 account(s) · 4 transaction(s) · goldens are exact micro-units, no floats.

## Invariants

Checked by recomputing the balances from the accepted transactions, independently of the code that maintains them.

| invariant | result |
|---|---|
| every transaction sums to zero | holds |
| balances match the transactions | holds |
| every balance sums to zero | holds (0.000000) |
| every account respects its declared minimum | holds |

0 violation(s).

## Balances

| account | kind | balance |
|---|---|---|
| `assets:bank` | asset | 128.950000 |
| `assets:receivable` | asset | 0.000000 |
| `expenses:fees` | expense | 1.050000 |
| `revenue:subscriptions` | revenue | -130.000000 |
## Reconciliation — clean

| measure | value |
|---|---|
| entries in the ledger | 4 |
| entries in the statement | 4 |
| references matched | 4 |
| final balance, ledger | 128.950000 |
| final balance, statement | 128.950000 |
| difference | 0.000000 |

**No divergence**: every entry agrees, amounts included.

## Reconciliation — divergent

| measure | value |
|---|---|
| entries in the ledger | 4 |
| entries in the statement | 4 |
| references matched | 3 |
| final balance, ledger | 128.950000 |
| final balance, statement | 128.500000 |
| difference | -0.450000 |

**First divergence at entry 3** (amount mismatch): fee_1: the ledger booked -1.050000 across 1 entry, the statement says -1.500000 across 1 entry

The running balances before it: ledger 130.000000, statement 130.000000 (difference 0.000000).

## Reconciliation — pending

| measure | value |
|---|---|
| entries in the ledger | 4 |
| entries in the statement | 3 |
| references matched | 3 |
| final balance, ledger | 128.950000 |
| final balance, statement | 148.950000 |
| difference | 20.000000 |

**First divergence at entry 2** (in the ledger, not in the statement (pending)): re_1: in the ledger (-20.000000), not in the statement

The running balances before it: ledger 150.000000, statement 150.000000 (difference 0.000000).

In the ledger, not in the statement (usually pending): re_1.

