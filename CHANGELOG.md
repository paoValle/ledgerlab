# Changelog

Format [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.1] - 2026-10-07

### Added
- `snapshot`: appends the whole state — the chart, the balances and the accepted transactions — as a
  `balances.snapshot` event that a replay can start from.
- `compact`: writes a new log with the newest snapshot and the events after it, refusing to overwrite
  an existing file and refusing to run without a snapshot. The input log is never modified, and the
  accepted transactions stay inside the snapshot so idempotency keeps holding after a compaction.
- Replay **checks** a snapshot instead of believing it: the transactions are numbered without gaps,
  each sums to zero and names accounts the snapshot opens, the idempotency keys are unique, and the
  balances are recomputed from the transactions and compared. A snapshot that does not add up is
  refused with `SnapshotInvalid`.

## [0.1.0] - 2026-10-05

First version: a double-entry ledger that proves its invariants and says where it stopped agreeing
with somebody else's statement.

### Added
- Money as signed `i64` micro-units, with strict parsing: more than six decimals is refused rather
  than rounded away, and a sum that would overflow is an error and not a very large number.
- Double entry enforced as an invariant (at least two postings, summing to zero), per-account
  minimum balances, and idempotency keys: the same request twice is a no-op, the same key with
  different money is a conflict.
- An append-only JSONL log, flushed with `sync_all` on every append, with torn writes detected by
  line number instead of skipped.
- Balances derived by replay from the log, and an audit that recomputes them from the transactions
  and compares — two paths that must agree.
- Reconciliation against an external statement that reports the **first divergence** and its reason
  (amount mismatch, missing from the ledger, pending in the ledger, out of order, length), because a
  single "difference" number collapses three different facts into a word that helps nobody.
- Six commands (`open`, `apply`, `balances`, `verify`, `reconcile`, `report`) and exit codes: `0`
  everything agrees, `1` a divergence that means the ledger is wrong, `2` usage or input error.
- Fourteen tests: five properties over generated books (`proptest`), five concrete cases, three for
  the money type, and a torn write detected by line number.

### Fixed
- `Amount::from_str` reported a range error for a syntax problem: `.5` was refused with "does not fit
  in 64 bits of micro-units", which named a range that was never in question (#1).
