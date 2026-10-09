//! The exit codes, which are the contract a CI gates on.
//!
//! The other suite is library-level: it never spawns a process, so nothing asserted what `main`
//! returns. A `1` that became a `0` would turn a red reconciliation green and no test would
//! notice — and the README promises exactly these codes ("`0` agrees, `1` a divergence that means
//! the ledger is wrong, `2` a usage or input error. A pending entry is reported and returns `0`").
//!
//! So these tests run the binary the way the README documents it, on the fixtures the README uses.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ledgerlab");

/// Runs the binary with `args` and returns what it did, stderr included.
fn ledgerlab(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("the test binary must be runnable")
}

/// The exit code, with the signal case named instead of swallowed.
fn code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("the binary must exit with a code, not a signal")
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ledgerlab-cli-{}-{name}", std::process::id()))
}

/// The ledger `make demo` builds: the chart, then the activity file applied once.
fn demo_log(name: &str) -> PathBuf {
    let path = temp_path(name);
    let log = path.to_str().expect("the temp path is utf-8");
    let opened = ledgerlab(&["open", "--log", log, "--accounts", "examples/chart.jsonl"]);
    assert!(opened.status.success(), "open: {opened:?}");
    let applied = ledgerlab(&[
        "apply",
        "--log",
        log,
        "--pending",
        "examples/activity.jsonl",
    ]);
    assert!(applied.status.success(), "apply: {applied:?}");
    path
}

#[test]
fn the_reconciliation_exit_codes_are_the_ones_the_readme_documents() {
    let path = demo_log("reconcile");
    let log = path.to_str().expect("the temp path is utf-8");
    let reconcile = |statement: &str| {
        ledgerlab(&[
            "reconcile",
            "--log",
            log,
            "--account",
            "assets:bank",
            "--statement",
            statement,
        ])
    };

    let clean = reconcile("examples/statement-clean.jsonl");
    assert_eq!(code(&clean), 0, "agreement is exit 0");

    let divergent = reconcile("examples/statement-divergent.jsonl");
    assert_eq!(
        code(&divergent),
        1,
        "a real divergence is exit 1: {}",
        String::from_utf8_lossy(&divergent.stdout)
    );

    let pending = reconcile("examples/statement-pending.jsonl");
    assert_eq!(
        code(&pending),
        0,
        "a timing difference is reported and returns 0: {}",
        String::from_utf8_lossy(&pending.stdout)
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn a_usage_error_is_exit_2_and_a_clean_ledger_verifies_with_0() {
    let path = demo_log("usage");
    let log = path.to_str().expect("the temp path is utf-8");
    assert_eq!(code(&ledgerlab(&["verify", "--log", log])), 0);
    std::fs::remove_file(&path).ok();

    assert_eq!(
        code(&ledgerlab(&["frobnicate"])),
        2,
        "an unknown command is a usage error"
    );
    assert_eq!(
        code(&ledgerlab(&["verify"])),
        2,
        "a missing --log is a usage error"
    );
    assert_eq!(
        code(&ledgerlab(&[])),
        2,
        "no command at all prints the usage and exits 2"
    );
}
