//! The command line: six commands, and exit codes a CI can gate on.
//!
//! ```console
//! ledgerlab open      --log ledger.jsonl --accounts chart.jsonl
//! ledgerlab apply     --log ledger.jsonl --pending activity.jsonl
//! ledgerlab balances  --log ledger.jsonl
//! ledgerlab verify    --log ledger.jsonl
//! ledgerlab reconcile --log ledger.jsonl --account assets:bank --statement bank.jsonl
//! ledgerlab report    --log ledger.jsonl --account assets:bank --statement clean=x.jsonl \
//!                     --statement divergent=y.jsonl --out reports/latest.md
//! ```
//!
//! Exit codes: `0` everything agrees, `1` an invariant failed or a statement diverged for a
//! reason that means the ledger is wrong, `2` usage or input error. The distinction matters:
//! `1` is a business fact to look at, `2` is a broken tool, and a CI that conflates them teaches
//! people to ignore it.

// The report builder is a wall of `format!` lines pushed into one string. `write!` would not be
// more correct here — writing into a `String` cannot fail — and it would bury the report's shape in
// `let _ =` noise, which is the one thing a reader of that function needs to see.
#![allow(clippy::format_push_string)]

use std::collections::BTreeMap;
use std::process::ExitCode;

use ledgerlab::reconcile::ReconciliationReason;
use ledgerlab::{
    audit, ledger::Event, reconcile, Account, Amount, Ledger, Log, Reconciliation, StatementEntry,
    Transaction,
};

const USAGE: &str = "\
ledgerlab — a double-entry ledger that proves its invariants

  open      --log <file> --accounts <chart.jsonl>
  apply     --log <file> --pending <transactions.jsonl>
  balances  --log <file> [--json]
  verify    --log <file> [--json]
  reconcile --log <file> --account <id> --statement <statement.jsonl> [--json]
  report    --log <file> --account <id> --statement <label=file>... [--out reports/latest.md]

Exit codes: 0 ok, 1 an invariant failed or a statement diverged, 2 usage or input error.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("ledgerlab: {message}");
            ExitCode::from(2)
        }
    }
}

/// Flags, as they were given on the command line.
#[derive(Debug, Default)]
struct Args {
    command: String,
    flags: BTreeMap<String, Vec<String>>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut args = Self::default();
        let mut rest = raw.iter();
        args.command = rest.next().cloned().unwrap_or_default();
        while let Some(token) = rest.next() {
            let Some(key) = token.strip_prefix("--") else {
                return Err(format!("unexpected argument {token:?}"));
            };
            if let Some((key, value)) = key.split_once('=') {
                args.flags
                    .entry(key.to_owned())
                    .or_default()
                    .push(value.to_owned());
                continue;
            }
            // every flag in this tool takes a value, except the ones we know are switches
            if matches!(key, "json") {
                args.flags
                    .entry(key.to_owned())
                    .or_default()
                    .push("true".to_owned());
                continue;
            }
            let value = rest
                .next()
                .ok_or_else(|| format!("--{key} needs a value"))?;
            args.flags
                .entry(key.to_owned())
                .or_default()
                .push(value.clone());
        }
        Ok(args)
    }

    fn one(&self, key: &str) -> Option<&str> {
        self.flags
            .get(key)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    fn many(&self, key: &str) -> &[String] {
        self.flags.get(key).map_or(&[], Vec::as_slice)
    }

    fn required(&self, key: &str) -> Result<&str, String> {
        self.one(key).ok_or_else(|| format!("--{key} is required"))
    }

    fn is_set(&self, key: &str) -> bool {
        self.flags.contains_key(key)
    }
}

fn run(raw: &[String]) -> Result<ExitCode, String> {
    let args = Args::parse(raw)?;
    if args.command.is_empty() || args.command == "help" {
        println!("{USAGE}");
        return Ok(ExitCode::from(if args.command.is_empty() { 2 } else { 0 }));
    }

    match args.command.as_str() {
        "open" => open(&args),
        "apply" => apply(&args),
        "balances" => balances(&args),
        "verify" => verify(&args),
        "reconcile" => reconcile_command(&args),
        "report" => report(&args),
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

/// Opens the accounts in a chart file.
fn open(args: &Args) -> Result<ExitCode, String> {
    let log = Log::new(args.required("log")?);
    let accounts: Vec<Account> = read_jsonl(args.required("accounts")?)?;
    let mut ledger = load(&log)?;

    let mut opened = 0;
    for account in accounts {
        if ledger
            .open(account.clone())
            .map_err(|error| error.to_string())?
        {
            opened += 1;
            log.append(&Event::AccountOpened { account })
                .map_err(|error| error.to_string())?;
        }
    }
    println!(
        "{opened} account(s) opened, {} now in the chart",
        ledger.accounts().len()
    );
    Ok(ExitCode::SUCCESS)
}

/// Applies the transactions in a file.
///
/// Applying the same file twice is a no-op, and that is the point: a rerun after a failure must
/// not double every charge.
fn apply(args: &Args) -> Result<ExitCode, String> {
    let log = Log::new(args.required("log")?);
    let pending: Vec<Transaction> = read_jsonl(args.required("pending")?)?;
    let mut ledger = load(&log)?;

    let (mut recorded, mut already) = (0, 0);
    for transaction in pending {
        match ledger.apply(transaction.clone()) {
            Ok(ledgerlab::Applied::Recorded(sequence)) => {
                log.append(&Event::TransactionApplied {
                    sequence,
                    transaction,
                })
                .map_err(|error| error.to_string())?;
                recorded += 1;
            }
            Ok(ledgerlab::Applied::AlreadyApplied) => already += 1,
            Err(error) => {
                // the ledger refuses the whole transaction and stays as it was: say which one and
                // stop, because continuing would apply a file that does not add up
                return Err(format!(
                    "{} ({}) refused: {error}",
                    transaction.id, transaction.idempotency_key
                ));
            }
        }
    }
    println!(
        "{recorded} transaction(s) applied, {already} already applied (idempotent), {} in the log",
        ledger.len()
    );
    Ok(ExitCode::SUCCESS)
}

/// Prints the balances.
fn balances(args: &Args) -> Result<ExitCode, String> {
    let ledger = load(&Log::new(args.required("log")?))?;
    if args.is_set("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(ledger.balances()).map_err(|error| error.to_string())?
        );
        return Ok(ExitCode::SUCCESS);
    }

    let mut total = Amount::ZERO;
    for (id, account) in ledger.accounts() {
        let balance = ledger.balance(id);
        total = total.checked_add(balance).unwrap_or(total);
        println!(
            "{:<28} {:<9} {:>14}",
            id,
            kind_label(account.kind),
            balance.to_string()
        );
    }
    println!(
        "{:<28} {:<9} {:>14}",
        "(sum of everything)",
        "",
        total.to_string()
    );
    Ok(ExitCode::SUCCESS)
}

/// Audits the ledger.
fn verify(args: &Args) -> Result<ExitCode, String> {
    let ledger = load(&Log::new(args.required("log")?))?;
    let report = audit(&ledger);

    if args.is_set("json") {
        let value = serde_json::json!({
            "accounts": report.accounts,
            "transactions": report.transactions,
            "global_sum": report.global_sum.to_string(),
            "recomputed_matches": report.recomputed_matches,
            "violations": report.violations.iter().map(|v| format!("{:?}: {}", v.kind, v.detail)).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "{} account(s), {} transaction(s), sum of everything {}, recomputed balances match: {}",
            report.accounts,
            report.transactions,
            report.global_sum,
            if report.recomputed_matches {
                "yes"
            } else {
                "NO"
            }
        );
        for violation in &report.violations {
            println!("  VIOLATION {:?}: {}", violation.kind, violation.detail);
        }
    }
    Ok(if report.passes() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Reconciles one account against one statement.
fn reconcile_command(args: &Args) -> Result<ExitCode, String> {
    let account = args.required("account")?;
    let ledger = load(&Log::new(args.required("log")?))?;
    let statement: Vec<StatementEntry> = read_jsonl(args.required("statement")?)?;
    let result = reconcile(&ledger, account, &statement);

    if args.is_set("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&reconciliation_json(&result))
                .map_err(|error| error.to_string())?
        );
    } else {
        println!("{}", render_reconciliation(&result, None));
    }
    Ok(if is_failing(&result) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Writes the artifact: audit, balances, and one reconciliation per statement.
fn report(args: &Args) -> Result<ExitCode, String> {
    let account = args.required("account")?;
    let ledger = load(&Log::new(args.required("log")?))?;
    let report = audit(&ledger);

    let mut markdown = String::new();
    markdown.push_str("# ledgerlab — the ledger, audited and reconciled\n\n");
    markdown.push_str(&format!(
        "Ledger: `{}` · {} account(s) · {} transaction(s) · goldens are exact micro-units, no floats.\n\n",
        args.required("log")?,
        report.accounts,
        report.transactions
    ));

    markdown.push_str("## Invariants\n\n");
    markdown.push_str("Checked by recomputing the balances from the accepted transactions, independently of the code that maintains them.\n\n");
    markdown.push_str("| invariant | result |\n|---|---|\n");
    markdown.push_str(&format!(
        "| every transaction sums to zero | {} |\n",
        if report
            .violations
            .iter()
            .any(|v| v.kind == ledgerlab::ViolationKind::UnbalancedTransaction)
        {
            "**FAILED**"
        } else {
            "holds"
        }
    ));
    markdown.push_str(&format!(
        "| balances match the transactions | {} |\n",
        if report.recomputed_matches {
            "holds"
        } else {
            "**FAILED**"
        }
    ));
    markdown.push_str(&format!(
        "| every balance sums to zero | {} ({}) |\n",
        if report.global_sum.is_zero() {
            "holds"
        } else {
            "**FAILED**"
        },
        report.global_sum
    ));
    markdown.push_str(&format!(
        "| every account respects its declared minimum | {} |\n",
        if report
            .violations
            .iter()
            .any(|v| v.kind == ledgerlab::ViolationKind::BelowMinimum)
        {
            "**FAILED**"
        } else {
            "holds"
        }
    ));
    markdown.push_str(&format!("\n{} violation(s).\n\n", report.violations.len()));

    markdown.push_str("## Balances\n\n| account | kind | balance |\n|---|---|---|\n");
    for (id, account_entry) in ledger.accounts() {
        markdown.push_str(&format!(
            "| `{id}` | {} | {} |\n",
            kind_label(account_entry.kind),
            ledger.balance(id)
        ));
    }

    let mut failing = !report.passes();
    for spec in args.many("statement") {
        let (label, path) = spec
            .split_once('=')
            .ok_or_else(|| format!("--statement wants label=file, got {spec:?}"))?;
        let statement: Vec<StatementEntry> = read_jsonl(path)?;
        let result = reconcile(&ledger, account, &statement);
        failing |= is_failing(&result);
        markdown.push_str(&render_reconciliation(&result, Some(label)));
        markdown.push('\n');
    }

    let out = args.one("out").unwrap_or("reports/latest.md");
    if let Some(parent) = std::path::Path::new(out).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    std::fs::write(out, &markdown).map_err(|error| format!("{out}: {error}"))?;
    println!("wrote {out}");
    print!("{markdown}");

    // a report that hides a divergence would be worse than no report
    Ok(if failing {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Whether a divergence means the ledger is wrong, as opposed to merely behind.
///
/// A pending entry is a timing difference and must not fail a build; a missing entry or a wrong
/// amount is money, and does.
fn is_failing(result: &Reconciliation) -> bool {
    match &result.first_divergence {
        None => false,
        Some(divergence) => match divergence.reason {
            ReconciliationReason::AmountMismatch
            | ReconciliationReason::MissingFromLedger
            | ReconciliationReason::OutOfOrder => true,
            ReconciliationReason::PendingInLedger => false,
            ReconciliationReason::LengthMismatch => {
                result.statement_entries > result.ledger_entries
            }
        },
    }
}

fn render_reconciliation(result: &Reconciliation, label: Option<&str>) -> String {
    let title = label.map_or_else(
        || format!("Reconciliation of `{}`", result.account),
        |label| format!("Reconciliation — {label}"),
    );
    let mut out = String::new();
    if label.is_some() {
        out.push_str(&format!("## {title}\n\n"));
    } else {
        out.push_str(&format!("{title}\n"));
    }
    out.push_str(&format!(
        "| measure | value |\n|---|---|\n| entries in the ledger | {} |\n| entries in the statement | {} |\n| references matched | {} |\n| final balance, ledger | {} |\n| final balance, statement | {} |\n| difference | {} |\n",
        result.ledger_entries,
        result.statement_entries,
        result.matched,
        result.ledger_final,
        result.statement_final,
        result.final_difference()
    ));

    match &result.first_divergence {
        None => out.push_str("\n**No divergence**: every entry agrees, amounts included.\n"),
        Some(divergence) => {
            out.push_str(&format!(
                "\n**First divergence at entry {}** ({}): {}\n\nThe running balances before it: ledger {}, statement {} (difference {}).\n",
                divergence.index,
                divergence.reason.label(),
                divergence.detail,
                divergence.ledger_running,
                divergence.statement_running,
                divergence.difference
            ));
        }
    }
    if !result.missing_from_ledger.is_empty() {
        out.push_str(&format!(
            "\nIn the statement, missing from the ledger: {}.\n",
            result.missing_from_ledger.join(", ")
        ));
    }
    if !result.pending_in_ledger.is_empty() {
        out.push_str(&format!(
            "\nIn the ledger, not in the statement (usually pending): {}.\n",
            result.pending_in_ledger.join(", ")
        ));
    }
    out
}

fn reconciliation_json(result: &Reconciliation) -> serde_json::Value {
    serde_json::json!({
        "account": result.account,
        "ledger_entries": result.ledger_entries,
        "statement_entries": result.statement_entries,
        "matched": result.matched,
        "clean": result.is_clean(),
        "ledger_final": result.ledger_final.to_string(),
        "statement_final": result.statement_final.to_string(),
        "difference": result.final_difference().to_string(),
        "missing_from_ledger": result.missing_from_ledger,
        "pending_in_ledger": result.pending_in_ledger,
        "first_divergence": result.first_divergence.as_ref().map(|divergence| serde_json::json!({
            "index": divergence.index,
            "reason": guard_format(format!("{:?}", divergence.reason)),
            "detail": divergence.detail,
            "ledger_running": divergence.ledger_running.to_string(),
            "statement_running": divergence.statement_running.to_string(),
        })),
    })
}

fn guard_format(value: String) -> String {
    value
}

fn kind_label(kind: ledgerlab::AccountKind) -> &'static str {
    match kind {
        ledgerlab::AccountKind::Asset => "asset",
        ledgerlab::AccountKind::Liability => "liability",
        ledgerlab::AccountKind::Equity => "equity",
        ledgerlab::AccountKind::Revenue => "revenue",
        ledgerlab::AccountKind::Expense => "expense",
    }
}

fn load(log: &Log) -> Result<Ledger, String> {
    let events = log.read().map_err(|error| error.to_string())?;
    Ledger::replay(&events).map_err(|error| error.to_string())
}

/// Reads a JSONL input file.
///
/// Blank lines and lines starting with `#` are skipped: **input** files are written by humans, and
/// a fixture that explains itself is worth more than a fixture that cannot. The event log is
/// deliberately stricter — it is written by this program, so a line that does not parse there is
/// damage, not a comment.
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &str) -> Result<Vec<T>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| format!("{path}:{}: {error}", index + 1))
        })
        .collect()
}
