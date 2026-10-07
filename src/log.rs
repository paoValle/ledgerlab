//! The event log: append-only, one JSON object per line, and honest about damage.
//!
//! The log is the only truth in this system. Balances are a cache rebuilt from it, which is why
//! two properties matter more than speed:
//!
//! - **append-only**: there is no `update` and no `delete`. Correcting a mistake means applying a
//!   correcting transaction, so the history of what somebody believed stays readable;
//! - **durable on write**: every append is flushed to the operating system with `sync_all`. An
//!   append-only log that loses its last line in a power cut is a ledger that is silently wrong,
//!   which is the worst of both worlds.
//!
//! A **torn write** — the last line cut in half by a crash — is detected by line number and
//! reported, never skipped. Skipping it would make the ledger quietly forget a transaction that
//! may already have been applied.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::ledger::Event;

/// An append-only JSONL log.
#[derive(Debug, Clone)]
pub struct Log {
    path: PathBuf,
}

impl Log {
    /// Points at a log file, which does not have to exist yet.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one event and flushes it to disk.
    pub fn append(&self, event: &Event) -> Result<(), LogError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| LogError::Io {
                    path: self.path.display().to_string(),
                    detail: error.to_string(),
                })?;
            }
        }
        let line =
            serde_json::to_string(event).map_err(|error| LogError::Encode(error.to_string()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| LogError::Io {
                path: self.path.display().to_string(),
                detail: error.to_string(),
            })?;
        writeln!(file, "{line}").map_err(|error| LogError::Io {
            path: self.path.display().to_string(),
            detail: error.to_string(),
        })?;
        // the difference between a ledger and a wish
        file.sync_all().map_err(|error| LogError::Io {
            path: self.path.display().to_string(),
            detail: error.to_string(),
        })
    }

    /// Writes a whole log to a path that does not exist yet.
    ///
    /// Compaction is the only caller, and it refuses to write over an existing file: the original
    /// log is the record of what happened, and a tool that overwrote it while compacting would
    /// destroy the evidence it was asked to read. The refusal is `create_new`, so it holds against
    /// two processes racing as well.
    pub fn write_new(&self, events: &[Event]) -> Result<(), LogError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| LogError::Io {
                    path: self.path.display().to_string(),
                    detail: error.to_string(),
                })?;
            }
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&self.path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    LogError::Exists {
                        path: self.path.display().to_string(),
                    }
                } else {
                    LogError::Io {
                        path: self.path.display().to_string(),
                        detail: error.to_string(),
                    }
                }
            })?;
        for event in events {
            let line = serde_json::to_string(event)
                .map_err(|error| LogError::Encode(error.to_string()))?;
            writeln!(file, "{line}").map_err(|error| LogError::Io {
                path: self.path.display().to_string(),
                detail: error.to_string(),
            })?;
        }
        file.sync_all().map_err(|error| LogError::Io {
            path: self.path.display().to_string(),
            detail: error.to_string(),
        })
    }

    /// Reads every event. A malformed or torn line is an error, with its line number.
    ///
    /// A log that does not exist yet is an **empty** log, not an error: the first command of a
    /// fresh ledger opens the accounts before anything was ever written.
    pub fn read(&self) -> Result<Vec<Event>, LogError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&self.path).map_err(|error| LogError::Io {
            path: self.path.display().to_string(),
            detail: error.to_string(),
        })?;
        let mut events = Vec::new();
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|error| LogError::Io {
                path: self.path.display().to_string(),
                detail: error.to_string(),
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(&line).map_err(|error| LogError::Line {
                line: index + 1,
                detail: error.to_string(),
            })?;
            events.push(event);
        }
        Ok(events)
    }
}

/// What can go wrong with the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogError {
    /// The file could not be read or written.
    Io {
        /// The path.
        path: String,
        /// What the operating system said.
        detail: String,
    },
    /// A line was not a valid event. A torn write lands here.
    Line {
        /// Which line, one-based.
        line: usize,
        /// What was wrong.
        detail: String,
    },
    /// An event could not be encoded.
    Encode(String),
    /// There is no snapshot to compact from.
    NothingToCompact,
    /// A file is already there, and this command does not overwrite a ledger.
    Exists {
        /// The path that is in the way.
        path: String,
    },
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, detail } => write!(f, "{path}: {detail}"),
            Self::Line { line, detail } => write!(
                f,
                "line {line} is not a valid event ({detail}). A torn write is not skipped: \
                 the transaction it was recording may already have been applied"
            ),
            Self::Encode(detail) => write!(f, "cannot encode the event: {detail}"),
            Self::NothingToCompact => write!(
                f,
                "there is no snapshot in this log, so there is nothing to compact from: run \
                 `ledgerlab snapshot --log <file>` first. Dropping the events without one would \
                 drop the history they are"
            ),
            Self::Exists { path } => write!(
                f,
                "{path} already exists, and a ledger is not overwritten: choose another --out, or \
                 remove the file yourself once you are sure"
            ),
        }
    }
}

impl std::error::Error for LogError {}

/// The events a compacted log carries: the newest snapshot, and everything after it.
///
/// This is the rule for what may be dropped, and it is the snapshot that makes it safe: a snapshot
/// is sufficient to rebuild the state — `Ledger::replay` checks that every time it uses one — so the
/// events it covers are redundant and the events after it are not. Without a snapshot there is
/// nothing to compact from, and this refuses instead of writing a log that lost its history.
///
/// What it does **not** buy is a log that stops growing: the snapshot carries the accepted
/// transactions, because idempotency is a promise about them and a log that cannot answer "was this
/// one of them" charges twice. Compaction buys a shorter file to read and a replay that starts from
/// a state instead of from the first event ever written.
pub fn compact(events: &[Event]) -> Result<Vec<Event>, LogError> {
    let Some(newest) = events
        .iter()
        .rposition(|event| matches!(event, Event::BalancesSnapshot { .. }))
    else {
        return Err(LogError::NothingToCompact);
    };
    Ok(events[newest..].to_vec())
}
