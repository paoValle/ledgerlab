//! The command-line contract: the exit codes, and what the two input formats accept.
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

    // The one that used to be green: a reference the ledger never saw, with something else still
    // pending in front of it. The walk is the ledger's order, so the pending reference is the
    // first divergence — and gating the exit code on the first one alone made the phantom entry
    // exit 0, which is the difference a CI is supposed to catch reported as agreement.
    let phantom = reconcile("examples/statement-phantom.jsonl");
    assert_eq!(
        code(&phantom),
        1,
        "money the bank moved and this ledger never saw is exit 1, \
         however much of the ledger is still pending: {}",
        String::from_utf8_lossy(&phantom.stdout)
    );
    let printed = String::from_utf8_lossy(&phantom.stdout);
    assert!(
        printed.contains("ghost_9"),
        "and the report names it: {printed}"
    );

    std::fs::remove_file(&path).ok();
}

/// The machine interface carries the same verdict as the exit code.
///
/// `clean` is `first_divergence.is_none()`, so it is `false` for a pending entry — the statement the
/// README says is *reported and returns `0`*. A consumer gating on it would fail the statement the
/// human report calls a timing difference; `failing` is the field that means what exit `1` means.
#[test]
fn the_json_verdict_is_the_exit_code() {
    let path = demo_log("json-verdict");
    let log = path.to_str().expect("the temp path is utf-8");
    let case = |statement: &str| {
        let output = ledgerlab(&[
            "reconcile",
            "--log",
            log,
            "--account",
            "assets:bank",
            "--statement",
            statement,
            "--json",
        ]);
        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("the output is JSON");
        let failing = json["failing"].as_bool().expect("failing is a boolean");
        (code(&output), failing)
    };

    assert_eq!(case("examples/statement-clean.jsonl"), (0, false));
    assert_eq!(
        case("examples/statement-pending.jsonl"),
        (0, false),
        "a timing difference exits 0 and must not read as failing"
    );
    assert_eq!(case("examples/statement-divergent.jsonl"), (1, true));
    assert_eq!(
        case("examples/statement-phantom.jsonl"),
        (1, true),
        "a reference the ledger never saw fails wherever it sits"
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

#[test]
fn the_json_verdict_of_verify_is_the_exit_code_too() {
    // the README's rule: `--json` carries the verdict the exit code gates on. `reconcile` did;
    // `verify` returned `violations` and `recomputed_matches` and left the consumer to derive
    // `Audit::passes` from them, which is the conflation the field exists to prevent. Only the
    // clean side is reachable from here — `apply` and `replay` refuse every condition the audit
    // reports — so the failing side is asserted in `src/main.rs` against a hand-built report.
    let path = demo_log("verify-json");
    let log = path.to_str().expect("the temp path is utf-8");
    let output = ledgerlab(&["verify", "--log", log, "--json"]);
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the output is JSON");
    assert_eq!(code(&output), 0, "the demo ledger is clean: {output:?}");
    assert_eq!(
        json["failing"].as_bool(),
        Some(false),
        "a clean ledger must not read as failing: {json}"
    );
    assert_eq!(json["violations"].as_array().map(Vec::len), Some(0));

    std::fs::remove_file(&path).ok();
}

#[test]
fn the_help_does_not_promise_a_flag_snapshot_never_reads() {
    // the usage line is the tool's own contract, and it said `snapshot --log <file> --out
    // <file>` while `snapshot` appends to the log and never reads `--out`: a flag that is
    // accepted and dropped is how somebody learns their backup went to the wrong place.
    let output = ledgerlab(&[]);
    let usage = String::from_utf8_lossy(&output.stdout);
    assert!(usage.contains("snapshot  --log <file>"), "{usage}");
    assert!(
        !usage.contains("snapshot  --log <file> --out"),
        "snapshot appends to the log; it takes no --out: {usage}"
    );
    assert_eq!(code(&output), 2, "no command is a usage error: {output:?}");
}

#[test]
fn a_flag_a_command_does_not_read_is_a_usage_error() {
    // `snapshot --log l --out backup.jsonl` used to exit 0, print a snapshot and write nothing:
    // the flag was accepted and dropped, which is how somebody learns their backup went nowhere.
    let path = demo_log("unknown-flag");
    let log = path.to_str().expect("the temp path is utf-8");
    let out = temp_path("not-written");
    let output = ledgerlab(&[
        "snapshot",
        "--log",
        log,
        "--out",
        out.to_str().expect("utf-8"),
    ]);
    assert_eq!(code(&output), 2, "a usage error, not silence: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--out"), "{stderr}");
    assert!(!out.exists(), "the flag must not have been half-honoured");

    // and a flag the command does read still works
    let output = ledgerlab(&["snapshot", "--log", log, "--json"]);
    assert_eq!(code(&output), 0, "{output:?}");

    std::fs::remove_file(&path).ok();
}

/// The README's two input formats: "Input files (charts, transactions, statements) are JSONL and
/// may contain `#` comments … The **event log** is stricter — it is written by this program, so a
/// line that does not parse there is damage, not a comment. A torn write is reported with its line
/// number and never skipped."
#[test]
fn input_files_take_a_comment_and_the_event_log_treats_one_as_damage() {
    let path = temp_path("comments");
    let log = path.to_str().expect("the temp path is utf-8");
    let activity = std::fs::read_to_string("examples/activity.jsonl").expect("read the fixture");
    let commented = temp_path("commented-activity");
    std::fs::write(
        &commented,
        format!("# a comment the input format allows\n\n{activity}"),
    )
    .expect("write the commented activity file");

    let opened = ledgerlab(&["open", "--log", log, "--accounts", "examples/chart.jsonl"]);
    assert!(opened.status.success(), "open: {opened:?}");
    let applied = ledgerlab(&[
        "apply",
        "--log",
        log,
        "--pending",
        commented.to_str().expect("the temp path is utf-8"),
    ]);
    assert!(
        applied.status.success(),
        "a comment must not stop an input file: {applied:?}"
    );
    assert!(
        String::from_utf8_lossy(&applied.stdout).contains("4 transaction(s) applied"),
        "and the transactions after it are the ones applied: {}",
        String::from_utf8_lossy(&applied.stdout)
    );

    // The same line in the log is damage, and it is reported where it is.
    let events = std::fs::read_to_string(&path)
        .expect("read the log")
        .lines()
        .count();
    let mut contents = std::fs::read_to_string(&path).expect("read the log");
    contents.push_str("# not a comment here\n");
    std::fs::write(&path, contents).expect("append the comment line");

    let verified = ledgerlab(&["verify", "--log", log]);
    assert_eq!(
        code(&verified),
        2,
        "a torn write is an input error, not a divergence: {verified:?}"
    );
    let stderr = String::from_utf8_lossy(&verified.stderr);
    assert!(
        stderr.contains(&format!("line {}", events + 1)),
        "the line number is what makes it findable: {stderr}"
    );
    assert!(stderr.contains("A torn write is not skipped"), "{stderr}");

    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&commented).ok();
}
