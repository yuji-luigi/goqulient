//! Query execution for the editor (`query/run`): a script is split (SQL) or parsed (Mongo shell), checked, and run
//! statement by statement on a session. The safety rules, in order:
//! - destructive statements (`danger`) run nothing until the caller confirms (skipped for read-only profiles, whose
//!   writes are refused anyway, as in the app);
//! - every statement passes the client read-only check (`sql::check_sql`, `shell::check_mongo`) before anything is
//!   sent; the server session stays read-only too (the drivers set it at connect);
//! - each statement goes through the driver's deadline, server-side cancel and `RowCollector` cap;
//! - the script stops at the first error, which is the last result.
use crate::drivers::mongo::shell::{self, MongoCall};
use crate::error::{DbError, Result};
use crate::limits::HARD_MAX_ROWS;
use crate::model::{QueryResult, ResultColumn};
use crate::profile::DatabaseKind;
use crate::session::DatabaseSession;
use serde::Serialize;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

pub mod danger;
pub mod sql;

pub use danger::Finding;

/// One statement as a driver runs it.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Sql(String),
    Mongo(Box<MongoCall>),
}

/// `query/run` options besides the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// The database to start in; the profile's when absent (PostgreSQL: `postgres`, MongoDB: `test` if it has none).
    pub database: Option<String>,
    /// The caller showed the destructive statements and the user agreed.
    pub confirmed: bool,
    /// Row cap per statement; the profile's `maxRows` when absent, never above `HARD_MAX_ROWS`.
    pub max_rows: Option<usize>,
}

/// Where a statement is in the script: UTF-16 code unit offsets (what editors count), `end` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResultKind {
    /// A row set (`columns`, `rows`; MongoDB also `documents`).
    Rows,
    /// A command that changed rows or documents (`affectedRows`).
    Affected,
    /// Neither (`USE`, `use x`, a rename): `message`, if any.
    Message,
    /// The statement failed (`error`); nothing after it ran.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementError {
    /// `DbError::display_message`: secrets removed.
    pub message: String,
    /// The `DbError` kind (`readOnly`, `timeout`, `query`, …), as in the protocol's error data.
    pub kind: &'static str,
}

/// The result of one statement.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementResult {
    pub kind: ResultKind,
    /// The statement as written (trimmed; SQL keeps its comments).
    pub statement: String,
    pub span: Span,
    /// The database it ran in (after `USE` / `use`).
    pub database: String,
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<Option<String>>>,
    /// More rows existed than `maxRows`; the rest wasn't read (the server was stopped).
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Server notices raised by the statement (PostgreSQL `NOTICE`/`WARNING`), capped.
    pub notices: Vec<String>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<StatementError>,
}

/// A destructive statement waiting for confirmation, with where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Confirmation {
    #[serde(flatten)]
    pub finding: Finding,
    pub span: Span,
}

/// What `query/run` answers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum RunOutcome {
    /// One result per statement that ran; the last one is the error when the script stopped early.
    Results { results: Vec<StatementResult> },
    /// Nothing ran: run again with `confirmed` once the user agreed.
    NeedsConfirmation { findings: Vec<Confirmation> },
}

struct Planned {
    text: String,
    span: Span,
    database: String,
    statement: Statement,
}

/// UTF-16 offset of every `char` index of `text` (and of its end).
fn utf16_offsets(text: &str) -> Vec<usize> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut at = 0;
    for c in text.chars() {
        out.push(at);
        at += c.len_utf16();
    }
    out.push(at);
    out
}

fn span(offsets: &[usize], start: usize, end: usize) -> Span {
    let last = offsets.last().copied().unwrap_or(0);
    Span { start: offsets.get(start).copied().unwrap_or(last), end: offsets.get(end).copied().unwrap_or(last) }
}

fn failed(planned: &Planned, e: &DbError, elapsed_ms: u64) -> StatementResult {
    StatementResult {
        kind: ResultKind::Error,
        statement: planned.text.clone(),
        span: planned.span,
        database: planned.database.clone(),
        columns: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected_rows: None,
        documents: None,
        message: None,
        notices: Vec::new(),
        elapsed_ms,
        error: Some(StatementError { message: e.display_message(), kind: e.kind() }),
    }
}

fn succeeded(planned: &Planned, r: QueryResult, elapsed_ms: u64) -> StatementResult {
    let kind = if !r.columns.is_empty() {
        ResultKind::Rows
    } else if r.affected_rows.is_some() {
        ResultKind::Affected
    } else {
        ResultKind::Message
    };
    StatementResult {
        kind,
        statement: planned.text.clone(),
        span: planned.span,
        database: planned.database.clone(),
        columns: r.columns,
        rows: r.rows,
        truncated: r.truncated,
        affected_rows: r.affected_rows,
        documents: r.documents,
        message: r.message,
        notices: r.notices,
        elapsed_ms,
        error: None,
    }
}

/// Runs a script on `session` (see the module docs for the order of the checks). Fails only when `cancel` fires
/// (`DbError::Cancelled`); every other failure is the last result. An empty script (or only comments) has no results.
pub async fn run(
    session: &dyn DatabaseSession,
    text: &str,
    options: &RunOptions,
    cancel: &CancellationToken,
) -> Result<RunOutcome> {
    let profile = session.profile();
    let max_rows = options.max_rows.unwrap_or(profile.max_rows.max(1) as usize).clamp(1, HARD_MAX_ROWS);
    let mut database = options.database.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| profile.database.clone());
    if database.is_empty() {
        match profile.kind {
            DatabaseKind::Postgres => database = "postgres".into(),
            DatabaseKind::Mongodb => database = "test".into(),
            DatabaseKind::Mysql => {} // the server's default, as the connection has none
        }
    }
    let offsets = utf16_offsets(text);
    let dialect = match profile.kind {
        DatabaseKind::Postgres => Some(sql::Dialect::Postgres),
        DatabaseKind::Mysql => Some(sql::Dialect::Mysql),
        DatabaseKind::Mongodb => None,
    };
    let planned: Vec<Planned> = match dialect {
        Some(dialect) => sql::split_tracking_database(text, dialect, &database)
            .into_iter()
            .map(|st| Planned {
                span: span(&offsets, st.start, st.end),
                database: st.database,
                statement: Statement::Sql(st.text.clone()),
                text: st.text,
            })
            .collect(),
        None => match shell::parse_script(text, &database) {
            Ok(statements) => statements
                .into_iter()
                .map(|st| Planned {
                    span: span(&offsets, st.start, st.end),
                    database: st.call.database(&st.database).to_owned(),
                    text: st.text,
                    statement: Statement::Mongo(Box::new(st.call)),
                })
                .collect(),
            // Nothing runs, so there is nothing to confirm either.
            Err(e) => {
                let whole = Planned {
                    text: text.trim().to_owned(),
                    span: Span { start: 0, end: offsets.last().copied().unwrap_or(0) },
                    database,
                    statement: Statement::Sql(String::new()),
                };
                return Ok(RunOutcome::Results { results: vec![failed(&whole, &e, 0)] });
            }
        },
    };
    if !options.confirmed && !profile.read_only {
        let findings: Vec<Confirmation> = planned
            .iter()
            .flat_map(|p| {
                let found = match (&p.statement, dialect) {
                    (Statement::Sql(s), Some(d)) => danger::sql_statement(s, d, &p.database),
                    (Statement::Mongo(call), _) => danger::mongo_statement(call, &p.text, &p.database),
                    (Statement::Sql(_), None) => Vec::new(),
                };
                found.into_iter().map(|finding| Confirmation { finding, span: p.span })
            })
            .collect();
        if !findings.is_empty() {
            return Ok(RunOutcome::NeedsConfirmation { findings });
        }
    }
    let mut results = Vec::with_capacity(planned.len());
    for p in &planned {
        if cancel.is_cancelled() {
            return Err(DbError::Cancelled);
        }
        let started = Instant::now();
        let checked = match (&p.statement, dialect) {
            (Statement::Sql(s), Some(d)) => sql::check_sql(profile, s, &p.database, d),
            (Statement::Mongo(call), _) => shell::check_mongo(profile, call, &p.database),
            (Statement::Sql(_), None) => Err(DbError::Unsupported("SQL on a MongoDB connection".into())),
        };
        let outcome = match checked {
            Ok(()) => session.run_statement(&p.database, &p.statement, max_rows, cancel).await,
            Err(e) => Err(e),
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(r) => results.push(succeeded(p, r, elapsed_ms)),
            Err(DbError::Cancelled) => return Err(DbError::Cancelled),
            Err(e) => {
                results.push(failed(p, &e, elapsed_ms));
                break;
            }
        }
    }
    Ok(RunOutcome::Results { results })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_count_utf16_units() {
        let o = utf16_offsets("a🇮🇹é");
        assert_eq!(o, [0, 1, 3, 5, 6]);
        assert_eq!(span(&o, 1, 9), Span { start: 1, end: 6 });
    }

    #[test]
    fn outcome_json_shape() {
        let v = serde_json::to_value(RunOutcome::Results { results: Vec::new() }).unwrap();
        assert_eq!(v, serde_json::json!({"outcome": "results", "results": []}));
        let finding = Finding {
            warning: "w".into(),
            action: "drops table".into(),
            object: None,
            database: "shop".into(),
            statement: "DROP TABLE t".into(),
        };
        let v = serde_json::to_value(RunOutcome::NeedsConfirmation {
            findings: vec![Confirmation { finding, span: Span { start: 0, end: 12 } }],
        })
        .unwrap();
        assert_eq!(v["outcome"], "needsConfirmation");
        assert_eq!(v["findings"][0]["span"], serde_json::json!({"start": 0, "end": 12}));
        assert_eq!(v["findings"][0]["action"], "drops table");
        assert!(v["findings"][0].get("object").is_none());
    }
}
