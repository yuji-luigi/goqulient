//! Port of GokuCore's `DangerCheck`: statements that are easy to regret, so `query/run` asks for confirmation first.
//! Decided on `Normalized` SQL / parsed shell calls, and fails closed: what can't be classified (`DO`, `CALL`,
//! `EXECUTE`, MySQL executable comments, untargeted Mongo writes, unparseable scripts) is flagged too. Checked against
//! `parity/danger`.
#![warn(clippy::indexing_slicing)]

use crate::drivers::mongo::shell::{self, MongoCall};
use crate::profile::DatabaseKind;
use crate::query::sql::{self, ANALYZE, Dialect, Normalized};
use bson::{Bson, Document};
use fancy_regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;
use unicode_segmentation::UnicodeSegmentation;

/// One statement that is easy to regret (`DangerFinding`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Shown in the confirmation.
    pub warning: String,
    /// What it does, as words ("drops table", "deletes every row in").
    pub action: String,
    /// The table / collection it acts on, when it can be told.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// The database it acts on: after `USE x` / `use x`, and `admin` for admin commands.
    pub database: String,
    /// The statement as written.
    pub statement: String,
}

/// A finding before its database is known.
struct Draft {
    warning: String,
    action: String,
    object: Option<String>,
}

impl Draft {
    fn new(warning: String, action: &str, object: Option<String>) -> Self {
        Self { warning, action: action.to_owned(), object }
    }

    fn at(self, database: &str, statement: &str) -> Finding {
        Finding {
            warning: self.warning,
            action: self.action,
            object: self.object,
            database: database.to_owned(),
            statement: statement.to_owned(),
        }
    }
}

/// `DangerCheck.findings(for:kind:database:)` over a whole script.
pub fn findings(script: &str, kind: DatabaseKind, database: &str) -> Vec<Finding> {
    match kind {
        DatabaseKind::Postgres | DatabaseKind::Mysql => {
            let dialect = if kind == DatabaseKind::Postgres { Dialect::Postgres } else { Dialect::Mysql };
            sql::split_tracking_database(script, dialect, database)
                .into_iter()
                .flat_map(|st| sql_statement(&st.text, dialect, &st.database))
                .collect()
        }
        DatabaseKind::Mongodb => match shell::parse_script(script, database) {
            Ok(statements) => {
                statements.iter().flat_map(|st| mongo_statement(&st.call, &st.text, &st.database)).collect()
            }
            Err(_) => vec![unclassified("the script doesn't parse", &display(script)).at(database, script)],
        },
    }
}

/// The findings of one SQL statement that runs in `database`.
pub fn sql_statement(statement: &str, dialect: Dialect, database: &str) -> Vec<Finding> {
    sql_findings(statement, dialect).into_iter().map(|f| f.at(database, statement)).collect()
}

/// The findings of one shell statement (`text` as written) when the script's database is `database`.
pub fn mongo_statement(call: &MongoCall, text: &str, database: &str) -> Vec<Finding> {
    mongo_findings(call, &display(text)).into_iter().map(|f| f.at(call.database(database), text)).collect()
}

/// The statement as the user wrote it, on one line and clipped to 80 characters, for warnings.
fn display(statement: &str) -> String {
    let one_line = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.grapheme_indices(true).nth(80) {
        Some((end, _)) => format!("{}…", one_line.get(..end).unwrap_or_default()),
        None => one_line,
    }
}

fn unclassified(why: &str, shown: &str) -> Draft {
    Draft::new(format!("Can't tell what this changes ({why}): {shown}"), "runs unchecked code in", None)
}

// MARK: SQL

#[expect(clippy::expect_used, reason = "the patterns are literals, compiled by the unit tests")]
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

/// Data-changing words nested in a `WITH` or `EXPLAIN ANALYZE` (not `FOR UPDATE`, `ON DELETE`). Two lookbehinds, as
/// fancy-regex needs each to have a fixed length.
static EMBEDDED: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)(?<!\bFOR )(?<!\bON )\b(DELETE|UPDATE|DROP|TRUNCATE|ALTER)\b"));
static WHERE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bWHERE\b"));
static DROP_WORD: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bDROP\b"));
// `name` captures the object; the optional `type` group names its kind (TABLE, VIEW, …).
static DROP: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"(?i)^\(*\s*DROP\s+(?P<type>MATERIALIZED\s+VIEW|FOREIGN\s+TABLE|\w+)\s+(?:CONCURRENTLY\s+)?(?:IF\s+EXISTS\s+)?(?P<name>[^\s,;()]+)",
    )
});
static TRUNCATE: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)^\(*\s*TRUNCATE\s+(?:TABLE\s+)?(?:ONLY\s+)?(?P<name>[^\s,;()]+)"));
static DELETE: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)^\(*\s*DELETE\s+(?:FROM\s+)?(?:ONLY\s+)?(?P<name>[^\s,;()]+)"));
static UPDATE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)^\(*\s*UPDATE\s+(?:ONLY\s+)?(?P<name>[^\s,;()]+)"));
static ALTER: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"(?i)^\(*\s*ALTER\s+(?P<type>\w+)\s+(?:IF\s+EXISTS\s+)?(?:ONLY\s+)?(?P<name>[^\s,;()]+)")
});

fn sql_findings(statement: &str, dialect: Dialect) -> Vec<Draft> {
    let n = Normalized::new(statement, dialect);
    let shown = display(statement);
    if n.has_executable_comment {
        return vec![unclassified("MySQL executable comment", &shown)];
    }
    let kw = n.leading_keyword();
    match kw.as_str() {
        "DO" | "CALL" | "EXECUTE" => vec![unclassified(&format!("{kw} runs code this check can't see"), &shown)],
        "WITH" => embedded(&n.text).iter().filter_map(|s| sql_finding(s, &shown)).collect(),
        "EXPLAIN" if ANALYZE.is_match(&n.text).unwrap_or(true) => {
            embedded(&n.text).iter().filter_map(|s| sql_finding(s, &shown)).collect()
        }
        "EXPLAIN" => Vec::new(),
        _ => sql_finding(&n.text, &shown).into_iter().collect(),
    }
}

/// Data-changing statements nested in a `WITH` or `EXPLAIN ANALYZE`, each up to the end of its parentheses.
fn embedded(text: &str) -> Vec<String> {
    EMBEDDED
        .find_iter(text)
        .filter_map(|m| {
            let rest = text.get(m.ok()?.start()..)?;
            let mut depth = 0i64;
            let mut end = rest.len();
            for (i, c) in rest.char_indices() {
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                    if depth < 0 {
                        end = i;
                        break;
                    }
                }
            }
            rest.get(..end).map(str::to_owned)
        })
        .collect()
}

/// Only the text outside parentheses, so a `WHERE` in a subquery doesn't count for the outer statement.
fn top_level(s: &str) -> String {
    let mut depth = 0i64;
    s.chars()
        .filter(|&c| {
            if c == '(' {
                depth += 1;
                false
            } else if c == ')' {
                depth -= 1;
                false
            } else {
                depth == 0
            }
        })
        .collect()
}

/// One statement in normalized form (comments and literals already gone, names unquoted).
fn sql_finding(statement: &str, shown: &str) -> Option<Draft> {
    let kw = sql::first_word(statement);
    let finding = |warning: String, action: &str, re: &Regex| {
        let caps = re.captures(statement).ok().flatten();
        let group = |name: &str| caps.as_ref().and_then(|c| c.name(name)).map(|m| m.as_str().to_owned());
        let kind =
            group("type").map(|t| format!(" {}", t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")));
        Draft::new(warning, &format!("{action}{}", kind.unwrap_or_default()), group("name"))
    };
    match kw.as_str() {
        "DROP" => Some(finding(format!("DROP: {shown}"), "drops", &DROP)),
        "TRUNCATE" => Some(finding(format!("TRUNCATE: {shown}"), "truncates table", &TRUNCATE)),
        "DELETE" | "UPDATE" => {
            if WHERE.is_match(&top_level(statement)).unwrap_or(false) {
                return None;
            }
            let (re, action) =
                if kw == "DELETE" { (&*DELETE, "deletes every row in") } else { (&*UPDATE, "updates every row in") };
            Some(finding(format!("{kw} without WHERE affects every row: {shown}"), action, re))
        }
        "ALTER" => DROP_WORD
            .is_match(&top_level(statement))
            .unwrap_or(true)
            .then(|| finding(format!("ALTER … DROP: {shown}"), "alters", &ALTER)),
        _ => None,
    }
}

// MARK: MongoDB

fn mongo_findings(call: &MongoCall, shown: &str) -> Vec<Draft> {
    match call {
        MongoCall::Delete { collection, filter, multi: true } if filter.is_empty() => vec![Draft::new(
            format!("Deletes every document in {collection}"),
            "deletes every document in",
            Some(collection.clone()),
        )],
        MongoCall::Update { collection, filter, multi: true, .. } if filter.is_empty() => vec![Draft::new(
            format!("Updates every document in {collection}"),
            "updates every document in",
            Some(collection.clone()),
        )],
        MongoCall::Aggregate { pipeline, .. } => pipeline_findings(pipeline),
        MongoCall::RenameCollection { to, drop_target: true, .. } => {
            vec![Draft::new(format!("Replaces collection {to} (dropTarget)"), "replaces collection", Some(to.clone()))]
        }
        MongoCall::Command(d) | MongoCall::AdminCommand(d) => {
            let found = command_findings(d);
            if found.is_empty() && call.is_write() && call.write_targets("").is_none() {
                return vec![unclassified(d.keys().next().map_or("command", String::as_str), shown)];
            }
            found
        }
        _ => Vec::new(),
    }
}

fn as_doc(v: Option<&Bson>) -> Option<Document> {
    match v? {
        Bson::Document(d) => Some(d.clone()),
        Bson::Array(a) => Some(a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect()),
        _ => None,
    }
}

fn command_findings(d: &Document) -> Vec<Draft> {
    let Some(key) = d.keys().next().map(String::as_str) else { return Vec::new() };
    let coll = d.get_str(key).ok().map(str::to_owned);
    let shown = coll.clone().unwrap_or_else(|| "?".into());
    let specs = |field: &str| -> Vec<Document> {
        as_doc(d.get(field)).map(|s| s.into_iter().filter_map(|(_, v)| as_doc(Some(&v))).collect()).unwrap_or_default()
    };
    // A spec without a `q` document matches everything.
    let matches_all = |spec: &Document| as_doc(spec.get("q")).is_none_or(|q| q.is_empty());
    match key {
        "drop" => vec![Draft::new(format!("Drops collection {shown}"), "drops collection", coll)],
        "dropDatabase" => vec![Draft::new("Drops the whole database".into(), "drops database", None)],
        "delete" if specs("deletes").iter().any(|s| matches_all(s) && shell::int(s.get("limit")) == Some(0)) => {
            vec![Draft::new(format!("Deletes every document in {shown}"), "deletes every document in", coll)]
        }
        "update" if specs("updates").iter().any(|s| matches_all(s) && s.get("multi") == Some(&Bson::Boolean(true))) => {
            vec![Draft::new(format!("Updates every document in {shown}"), "updates every document in", coll)]
        }
        "aggregate" => pipeline_findings(
            &as_doc(d.get("pipeline")).map(|p| p.into_iter().map(|(_, v)| v).collect::<Vec<_>>()).unwrap_or_default(),
        ),
        "renameCollection" if d.get("dropTarget") == Some(&Bson::Boolean(true)) => {
            let to = d.get_str("to").ok().map(|t| t.split_once('.').map_or(t, |(_, c)| c).to_owned());
            let name = to.clone().unwrap_or_else(|| "?".into());
            vec![Draft::new(format!("Replaces collection {name} (dropTarget)"), "replaces collection", to)]
        }
        "mapReduce" if !as_doc(d.get("out")).is_some_and(|o| o.contains_key("inline")) => {
            let out = d.get_str("out").ok().map(str::to_owned).or_else(|| {
                let o = as_doc(d.get("out"))?;
                ["replace", "merge", "reduce"].iter().find_map(|k| o.get(*k))?.as_str().map(str::to_owned)
            });
            let name = out.clone().unwrap_or_else(|| "?".into());
            vec![Draft::new(format!("mapReduce writes into collection {name}"), "overwrites collection", out)]
        }
        _ => Vec::new(),
    }
}

/// `'x'`, `{ coll: 'x' }`, `{ into: 'x' }` or `{ into: { db, coll: 'x' } }`.
fn collection_name(v: Option<&Bson>) -> Option<String> {
    if let Some(Bson::String(s)) = v {
        return Some(s.clone());
    }
    let d = as_doc(v)?;
    collection_name(d.get("into")).or_else(|| d.get_str("coll").ok().map(str::to_owned))
}

/// `$out` replaces its target; `$merge` can overwrite documents in it.
fn pipeline_findings(pipeline: &[Bson]) -> Vec<Draft> {
    let mut out = Vec::new();
    for stage in pipeline.iter().filter_map(|s| as_doc(Some(s))) {
        for (key, warning, action) in [
            ("$out", "Replaces collection", "replaces collection"),
            ("$merge", "Merges into collection", "merges into collection"),
        ] {
            if let Some(target) = stage.get(key) {
                let name = collection_name(Some(target));
                let shown = name.clone().unwrap_or_else(|| "?".into());
                out.push(Draft::new(format!("{warning} {shown} ({key})"), action, name));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagged(script: &str, kind: DatabaseKind) -> bool {
        !findings(script, kind, "shop").is_empty()
    }

    #[test]
    fn every_pattern_compiles() {
        for re in [&*EMBEDDED, &*WHERE, &*DROP_WORD, &*DROP, &*TRUNCATE, &*DELETE, &*UPDATE, &*ALTER] {
            assert!(re.is_match("x").is_ok());
        }
    }

    #[test]
    fn sql_like_gokucore() {
        assert!(flagged("DELETE FROM orders -- WHERE id=1", DatabaseKind::Postgres));
        assert!(!flagged("DELETE FROM orders WHERE id = 1", DatabaseKind::Postgres));
        assert!(!flagged("SELECT * FROM t FOR UPDATE", DatabaseKind::Postgres));
        let f = findings("WITH d AS (DELETE FROM orders RETURNING 1) SELECT 1", DatabaseKind::Postgres, "shop");
        assert_eq!((f[0].action.as_str(), f[0].object.as_deref()), ("deletes every row in", Some("orders")));
        let f = findings("DROP MATERIALIZED VIEW IF EXISTS mv", DatabaseKind::Postgres, "shop");
        assert_eq!((f[0].action.as_str(), f[0].object.as_deref()), ("drops materialized view", Some("mv")));
    }

    #[test]
    fn mongo_like_gokucore() {
        assert!(flagged("db.t.deleteMany({})", DatabaseKind::Mongodb));
        assert!(!flagged("db.t.deleteMany({ a: 1 })", DatabaseKind::Mongodb));
        assert!(flagged("db.t.find(", DatabaseKind::Mongodb), "unparseable scripts fail closed");
        let f = findings("use x; db.adminCommand({ dropDatabase: 1 })", DatabaseKind::Mongodb, "shop");
        assert_eq!(f[0].database, "admin");
    }

    #[test]
    fn warnings_are_clipped() {
        assert_eq!(display("a\n  b"), "a b");
        assert_eq!(display(&"x".repeat(90)), format!("{}…", "x".repeat(80)));
    }
}
