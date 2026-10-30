//! SQL text for the query editor: the statement splitter (port of GokuCore's `SQLStatementStream` / `SQLSplitter`), the
//! shared lexical rules (`SQLLex`), `NormalizedSQL` (`SQLNormalizer.swift`), and the read-only checks built on it
//! (`SQLSplitter.isPotentiallyWriting`, `ConnectionProfile.checkSQL` in `AccessRules.swift`). Checked against
//! `parity/sql-split` and `parity/sql-safety`. Everything works on Unicode scalars (`char`), like the Swift code, and
//! fails closed: a pattern that can't be evaluated counts as a match (writing, refused).
#![warn(clippy::indexing_slicing)]

use crate::error::{DbError, Result};
use crate::profile::{ConnectionProfile, ReadOnlyRule};
use fancy_regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Mysql,
}

/// One statement of a script: its text (trimmed, comments kept) and where it is in the script, in `char` indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStatement {
    pub text: String,
    /// The database it runs in: the script's, or the last `USE` before it (MySQL).
    pub database: String,
    pub start: usize,
    pub end: usize,
}

// MARK: - Lexical rules (SQLLex)

fn at(s: &[char], i: usize) -> Option<char> {
    s.get(i).copied()
}

pub(crate) fn is_word(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c.is_ascii_digit() || c == '$'
}

/// `c` opens a quoted token; the result says whether backslash escapes inside it. PostgreSQL `E'…'` allows them, but
/// only when the `E` is a prefix, not the end of a word (`WHERE'\'` is `WHERE` followed by a standard string).
fn quote(c: char, previous: Option<char>, before_previous: Option<char>, dialect: Dialect) -> Option<bool> {
    if !(c == '\'' || c == '"' || (dialect == Dialect::Mysql && c == '`')) {
        return None;
    }
    if dialect == Dialect::Mysql {
        return Some(c != '`');
    }
    Some(c == '\'' && matches!(previous, Some('E' | 'e')) && !before_previous.is_some_and(is_word))
}

fn is_identifier_quote(q: char, dialect: Dialect) -> bool {
    if dialect == Dialect::Postgres { q == '"' } else { q == '`' }
}

/// `--` (MySQL: only before whitespace, a control character or the end, so `1--1` is arithmetic) or MySQL `#`.
fn starts_line_comment(s: &[char], i: usize, dialect: Dialect) -> bool {
    if dialect == Dialect::Mysql && at(s, i) == Some('#') {
        return true;
    }
    if at(s, i) != Some('-') || at(s, i + 1) != Some('-') {
        return false;
    }
    match (dialect, at(s, i + 2)) {
        (Dialect::Mysql, Some(c)) => c.is_whitespace() || (c as u32) < 0x20,
        _ => true,
    }
}

fn starts_block_comment(s: &[char], i: usize) -> bool {
    at(s, i) == Some('/') && at(s, i + 1) == Some('*')
}

/// Length of a MySQL `/*!` or MariaDB `/*M!` opener at `i` (code the server runs, not a comment).
fn executable_comment_opener(s: &[char], i: usize, dialect: Dialect) -> Option<usize> {
    if dialect != Dialect::Mysql || !starts_block_comment(s, i) {
        return None;
    }
    match at(s, i + 2)? {
        '!' => Some(3),
        'M' | 'm' if at(s, i + 3) == Some('!') => Some(4),
        _ => None,
    }
}

/// Length of a PostgreSQL `$tag$` opening a dollar-quoted string at `i` (`$1` parameters and `a$b` identifiers aren't).
fn dollar_tag(s: &[char], i: usize, previous: Option<char>) -> Option<usize> {
    if at(s, i) != Some('$') || previous.is_some_and(is_word) {
        return None;
    }
    let mut j = i + 1;
    while let Some(c) = at(s, j)
        && j - i < 64
        && (c.is_alphabetic() || c == '_' || (j > i + 1 && c.is_ascii_digit()))
    {
        j += 1;
    }
    (at(s, j) == Some('$')).then_some(j - i + 1)
}

fn matches_at(s: &[char], i: usize, seq: &[char]) -> bool {
    s.get(i..i + seq.len()) == Some(seq)
}

fn text(s: &[char], a: usize, b: usize) -> String {
    s.get(a..b.min(s.len())).unwrap_or_default().iter().collect()
}

/// `CharacterSet.whitespaces`: Unicode Zs and tab, so no line breaks.
fn is_blank(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

// MARK: - Splitter (SQLStatementStream over the whole script)

enum State {
    Normal,
    LineComment,
    BlockComment(usize),
    Quote(char, bool),
    Dollar(Vec<char>),
}

/// Splits a script into statements, respecting quotes, comments, PostgreSQL dollar quoting and MySQL `DELIMITER`
/// directives and `/*! … */` executable comments, like `SQLSplitter.split`. Statements that are only comments are dropped.
pub fn split(script: &str, dialect: Dialect) -> Vec<SplitStatement> {
    let s: Vec<char> = script.chars().collect();
    let n = s.len();
    let mut out = Vec::new();
    let mut state = State::Normal;
    let mut i = 0;
    // The statement being read is always the contiguous range `start..i` of the script.
    let mut start: Option<usize> = None;
    let mut has_code = false;
    let mut line_has_content = false;
    let mut delimiter = vec![';'];
    let flush = |start: &mut Option<usize>, has_code: &mut bool, end: usize, out: &mut Vec<SplitStatement>| {
        if let Some(a) = start.take()
            && *has_code
            && let Some(range) = s.get(a..end)
        {
            let lead = range.iter().take_while(|c| c.is_whitespace()).count();
            let trail = range.iter().rev().take_while(|c| c.is_whitespace()).count();
            if lead < range.len() {
                let (a, b) = (a + lead, end - trail);
                out.push(SplitStatement { text: text(&s, a, b), database: String::new(), start: a, end: b });
            }
        }
        *has_code = false;
    };
    // Appends `s[i]` to the statement being read.
    let append = |i: usize, start: &mut Option<usize>, line_has_content: &mut bool| {
        start.get_or_insert(i);
        match at(&s, i) {
            Some('\n') => *line_has_content = false,
            Some(c) if !c.is_whitespace() => *line_has_content = true,
            _ => {}
        }
    };
    while let Some(c) = at(&s, i) {
        match &mut state {
            State::LineComment => {
                append(i, &mut start, &mut line_has_content);
                i += 1;
                if c == '\n' {
                    state = State::Normal;
                }
            }
            State::BlockComment(depth) => {
                if c == '*' && at(&s, i + 1) == Some('/') {
                    append(i, &mut start, &mut line_has_content);
                    append(i + 1, &mut start, &mut line_has_content);
                    i += 2;
                    state = if *depth > 1 && dialect == Dialect::Postgres {
                        State::BlockComment(*depth - 1)
                    } else {
                        State::Normal
                    };
                } else if dialect == Dialect::Postgres && c == '/' && at(&s, i + 1) == Some('*') {
                    append(i, &mut start, &mut line_has_content);
                    append(i + 1, &mut start, &mut line_has_content);
                    i += 2;
                    *depth += 1;
                } else {
                    append(i, &mut start, &mut line_has_content);
                    i += 1;
                }
            }
            State::Quote(q, backslash) => {
                let (q, backslash) = (*q, *backslash);
                append(i, &mut start, &mut line_has_content);
                i += 1;
                if backslash && c == '\\' && i < n {
                    append(i, &mut start, &mut line_has_content);
                    i += 1;
                } else if c == q {
                    if at(&s, i) == Some(q) {
                        append(i, &mut start, &mut line_has_content); // doubled quote
                        i += 1;
                    } else {
                        state = State::Normal;
                    }
                }
            }
            State::Dollar(tag) => {
                if matches_at(&s, i, tag) {
                    for k in i..i + tag.len() {
                        append(k, &mut start, &mut line_has_content);
                    }
                    i += tag.len();
                    state = State::Normal;
                } else {
                    append(i, &mut start, &mut line_has_content);
                    i += 1;
                }
            }
            State::Normal => {
                // MySQL client DELIMITER directive (a whole line).
                if dialect == Dialect::Mysql && !line_has_content && (c == 'D' || c == 'd') {
                    let upper: Vec<char> = s
                        .get(i..(i + 10).min(n))
                        .unwrap_or_default()
                        .iter()
                        .map(|c| {
                            let mut u = c.to_uppercase();
                            match (u.next(), u.next()) {
                                (Some(one), None) => one,
                                _ => *c,
                            }
                        })
                        .collect();
                    if upper.len() >= 10
                        && upper.get(..9) == Some(&['D', 'E', 'L', 'I', 'M', 'I', 'T', 'E', 'R'][..])
                        && upper.get(9) == Some(&' ')
                    {
                        let nl = (i..n).find(|&k| at(&s, k) == Some('\n')).unwrap_or(n);
                        flush(&mut start, &mut has_code, i, &mut out);
                        let d = text(&s, i + 10, nl);
                        let d = d.trim_matches(is_blank);
                        if !d.is_empty() {
                            delimiter = d.chars().collect();
                        }
                        i = nl;
                        line_has_content = false;
                        continue;
                    }
                }
                let previous = start.filter(|&a| i > a).and_then(|_| i.checked_sub(1)).and_then(|k| at(&s, k));
                let before_previous =
                    start.filter(|&a| i > a + 1).and_then(|_| i.checked_sub(2)).and_then(|k| at(&s, k));
                if starts_line_comment(&s, i, dialect) {
                    state = State::LineComment;
                    append(i, &mut start, &mut line_has_content);
                    i += 1;
                    continue;
                }
                if starts_block_comment(&s, i) {
                    // MySQL executable comment: code for the server, keep the statement.
                    if executable_comment_opener(&s, i, dialect).is_some() {
                        has_code = true;
                    }
                    state = State::BlockComment(1);
                    append(i, &mut start, &mut line_has_content);
                    append(i + 1, &mut start, &mut line_has_content);
                    i += 2;
                    continue;
                }
                if let Some(backslash) = quote(c, previous, before_previous, dialect) {
                    state = State::Quote(c, backslash);
                    has_code = true;
                    append(i, &mut start, &mut line_has_content);
                    i += 1;
                    continue;
                }
                if dialect == Dialect::Postgres
                    && let Some(len) = dollar_tag(&s, i, previous)
                {
                    for k in i..i + len {
                        append(k, &mut start, &mut line_has_content);
                    }
                    state = State::Dollar(s.get(i..i + len).unwrap_or_default().to_vec());
                    i += len;
                    has_code = true;
                    continue;
                }
                if matches_at(&s, i, &delimiter) {
                    flush(&mut start, &mut has_code, i, &mut out);
                    i += delimiter.len();
                    continue;
                }
                if !c.is_whitespace() {
                    has_code = true;
                }
                append(i, &mut start, &mut line_has_content);
                i += 1;
            }
        }
    }
    flush(&mut start, &mut has_code, n, &mut out);
    out
}

/// `SQLSplitter.splitTrackingDatabase`: each statement with the database it runs in, following `USE` (MySQL).
pub fn split_tracking_database(script: &str, dialect: Dialect, database: &str) -> Vec<SplitStatement> {
    let mut db = database.to_owned();
    split(script, dialect)
        .into_iter()
        .map(|mut st| {
            st.database = db.clone();
            if let Some(next) = use_target(&st.text, dialect) {
                db = next;
            }
            st
        })
        .collect()
}

/// The statement from its first keyword on: leading whitespace, comments and parentheses removed.
pub fn strip_leading_comments(statement: &str) -> &str {
    let mut s = statement;
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
        if s.starts_with("--") || s.starts_with('#') {
            let Some(nl) = s.find('\n') else { return "" };
            s = s.get(nl + 1..).unwrap_or_default();
        } else if s.starts_with("/*") {
            let Some(end) = s.find("*/") else { return "" };
            s = s.get(end + 2..).unwrap_or_default();
        } else {
            return s;
        }
    }
}

/// First word, uppercased (whitespace and parentheses skipped).
pub fn first_word(s: &str) -> String {
    s.trim_start_matches(|c: char| c.is_whitespace() || c == '(')
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect::<String>()
        .to_uppercase()
}

/// First keyword of a statement, uppercased, skipping comments and parentheses.
pub fn leading_keyword(statement: &str) -> String {
    first_word(strip_leading_comments(statement))
}

#[expect(clippy::expect_used, reason = "the patterns are literals, compiled by the unit tests")]
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

/// Case-insensitive search; a pattern that fails at run time (backtracking limit) counts as found (fail closed).
fn found(re: &Regex, text: &str) -> bool {
    re.is_match(text).unwrap_or(true)
}

static USE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)^USE\s+(`(?:[^`]|``)+`|[^\s`;]+)$"));
static RETURNING: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bRETURNING\b"));

/// The database a MySQL `USE db` statement switches to.
pub fn use_target(statement: &str, dialect: Dialect) -> Option<String> {
    if dialect != Dialect::Mysql {
        return None;
    }
    let body = strip_leading_comments(statement).trim_matches(|c| matches!(c, ';' | ' ' | '\n' | '\t' | '\r'));
    if !USE.is_match(body).unwrap_or(false) {
        return None;
    }
    let name = body.get(3..)?.trim_matches(is_blank);
    let Some(quoted) = name.strip_prefix('`') else { return Some(name.to_owned()) };
    Some(quoted.strip_suffix('`').unwrap_or(quoted).replace("``", "`"))
}

/// Statements that return a row set (as opposed to a command tag).
pub fn returns_rows(statement: &str) -> bool {
    const ROWS: &[&str] = &[
        "SELECT", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "VALUES", "TABLE", "WITH", "FETCH", "CALL", "CHECK",
        "OPTIMIZE", "ANALYZE", "REPAIR", "HELP",
    ];
    ROWS.contains(&leading_keyword(statement).as_str()) || RETURNING.is_match(statement).unwrap_or(false)
}

// MARK: - NormalizedSQL

/// One SQL statement reduced to what the server treats as code, for safety checks that search it with regexes:
/// comments removed (a space remains) except MySQL `/*! … */`, whose content is kept; string literals (`E''`, `U&''`,
/// `$$…$$`, MySQL `"…"`) become `'?'`; quoted identifiers are unquoted (characters that can't be in a plain name become
/// `_`, a quoted keyword gets a trailing `_`, so `"where"` never reads as `WHERE`); whitespace collapsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    pub text: String,
    /// The statement used MySQL executable comments; checks that must be certain treat it as unclassifiable.
    pub has_executable_comment: bool,
}

/// Keywords the checks rely on; quoted identifiers spelling them get a `_` suffix.
const KEYWORDS: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "TRUNCATE",
    "DROP",
    "ALTER",
    "CREATE",
    "WITH",
    "WHERE",
    "INTO",
    "FROM",
    "SET",
    "EXPLAIN",
    "ANALYZE",
    "ANALYSE",
    "RETURNING",
    "TABLE",
    "VALUES",
    "OUTFILE",
    "DUMPFILE",
    "USE",
    "DO",
    "CALL",
    "BEGIN",
    "START",
    "COMMIT",
    "ROLLBACK",
];

impl Normalized {
    pub fn new(statement: &str, dialect: Dialect) -> Self {
        let s: Vec<char> = statement.chars().collect();
        let mut out = String::with_capacity(statement.len());
        let mut executable = false;
        let mut open_executable = 0usize;
        let mut i = 0;
        fn space(out: &mut String) {
            if out.chars().next_back().is_some_and(|c| c != ' ') {
                out.push(' ');
            }
        }
        const LITERAL: &str = "'?'";
        while let Some(c) = at(&s, i) {
            let previous = i.checked_sub(1).and_then(|k| at(&s, k));
            if starts_line_comment(&s, i, dialect) {
                while at(&s, i).is_some_and(|c| c != '\n') {
                    i += 1;
                }
                space(&mut out);
            } else if let Some(opener) = executable_comment_opener(&s, i, dialect) {
                executable = true;
                open_executable += 1;
                i += opener;
                while at(&s, i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1; // version: /*!50001
                }
                space(&mut out);
            } else if open_executable > 0 && c == '*' && at(&s, i + 1) == Some('/') {
                open_executable -= 1;
                i += 2;
                space(&mut out);
            } else if starts_block_comment(&s, i) {
                let mut depth = 0usize;
                loop {
                    if starts_block_comment(&s, i) && (depth == 0 || dialect == Dialect::Postgres) {
                        depth += 1;
                        i += 2;
                    } else if at(&s, i) == Some('*') && at(&s, i + 1) == Some('/') {
                        depth = depth.saturating_sub(1);
                        i += 2;
                    } else {
                        i += 1;
                    }
                    if depth == 0 || i >= s.len() {
                        break;
                    }
                }
                space(&mut out);
            } else if dialect == Dialect::Postgres
                && let Some(len) = dollar_tag(&s, i, previous)
            {
                let tag = s.get(i..i + len).unwrap_or_default();
                i += len;
                while i < s.len() && !(at(&s, i) == Some('$') && matches_at(&s, i, tag)) {
                    i += 1;
                }
                i = s.len().min(i + len);
                out.push_str(LITERAL);
            } else if dialect == Dialect::Postgres
                && (c == 'U' || c == 'u')
                && !previous.is_some_and(is_word)
                && at(&s, i + 1) == Some('&')
                && matches!(at(&s, i + 2), Some('"' | '\''))
            {
                let (content, end) = quoted(&s, i + 2, false);
                let (escape, after) = uescape(&s, end);
                if at(&s, i + 2) == Some('"') {
                    out.push_str(&identifier(&unicode_unescaped(&content, escape)));
                } else {
                    out.push_str(LITERAL);
                }
                i = after;
            } else if let Some(backslash) = quote(c, previous, i.checked_sub(2).and_then(|k| at(&s, k)), dialect) {
                let (content, end) = quoted(&s, i, backslash);
                if is_identifier_quote(c, dialect) {
                    out.push_str(&identifier(&content));
                } else {
                    out.push_str(LITERAL);
                }
                i = end;
            } else if c.is_whitespace() {
                space(&mut out);
                i += 1;
            } else {
                out.push(c);
                i += 1;
            }
        }
        Self { text: out.trim_matches(is_blank).to_owned(), has_executable_comment: executable }
    }

    /// First keyword, uppercased (parentheses skipped).
    pub fn leading_keyword(&self) -> String {
        first_word(&self.text)
    }

    fn contains(&self, re: &Regex) -> bool {
        found(re, &self.text)
    }
}

/// Content of the quoted token opening at `i`, and the index after its closing quote. Doubled quotes are one quote.
fn quoted(s: &[char], i: usize, backslash: bool) -> (String, usize) {
    let q = at(s, i);
    let mut content = String::new();
    let mut j = i + 1;
    while let Some(c) = at(s, j) {
        if backslash
            && c == '\\'
            && let Some(next) = at(s, j + 1)
        {
            content.push(next);
            j += 2;
            continue;
        }
        if Some(c) == q {
            if at(s, j + 1) == q {
                content.push(c);
                j += 2;
                continue;
            }
            return (content, j + 1);
        }
        content.push(c);
        j += 1;
    }
    (content, j)
}

/// A quoted name as a plain word that can't be mistaken for a keyword.
pub(crate) fn identifier(name: &str) -> String {
    let word: String = name.chars().map(|c| if is_word(c) { c } else { '_' }).collect();
    if KEYWORDS.contains(&word.to_uppercase().as_str()) {
        word + "_"
    } else if word.is_empty() {
        "_".to_owned()
    } else {
        word
    }
}

/// An optional `UESCAPE 'c'` clause after a `U&` token ending at `end`: the escape character and the index after it.
fn uescape(s: &[char], end: usize) -> (char, usize) {
    let mut j = end;
    while at(s, j).is_some_and(char::is_whitespace) {
        j += 1;
    }
    if text(s, j, j + 7).to_uppercase() != "UESCAPE" {
        return ('\\', end);
    }
    j += 7;
    while at(s, j).is_some_and(char::is_whitespace) {
        j += 1;
    }
    match (at(s, j), at(s, j + 1), at(s, j + 2)) {
        (Some('\''), Some(e), Some('\'')) => (e, j + 3),
        _ => ('\\', end),
    }
}

/// PostgreSQL `U&` escapes with escape character `e`: `eXXXX`, `e+XXXXXX`, `ee`.
fn unicode_unescaped(s: &str, e: char) -> String {
    let a: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while let Some(c) = at(&a, i) {
        if c == e
            && let Some(next) = at(&a, i + 1)
        {
            if next == e {
                out.push(e);
                i += 2;
                continue;
            }
            let plus = next == '+';
            let (start, length) = if plus { (i + 2, 6) } else { (i + 1, 4) };
            if let Some(hex) = a.get(start..start + length)
                && let Ok(v) = u32::from_str_radix(&hex.iter().collect::<String>(), 16)
                && let Some(u) = char::from_u32(v)
            {
                out.push(u);
                i = start + length;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

// MARK: - Read-only checks

static WRITES: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)\b(INSERT|UPDATE|DELETE|MERGE|TRUNCATE|DROP|ALTER|CREATE)\b"));
// SELECT … INTO new_table (PG), INTO OUTFILE / DUMPFILE (MySQL); `INTO @var` only sets a variable.
static INTO: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bINTO\s+(?!@)\w"));
pub(crate) static ANALYZE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bANALY[SZ]E\b"));

const READ_ONLY_KEYWORDS: &[&str] = &[
    "SELECT", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "VALUES", "TABLE", "WITH", "SET", "USE", "BEGIN", "START",
    "COMMIT", "ROLLBACK", "HELP",
];

/// Statements that may modify data or schema (or write server files): an allowlist of leading keywords, plus
/// data-modifying CTEs, `EXPLAIN ANALYZE` of a write, and `SELECT … INTO`. Decided on `Normalized`, so comments,
/// literals and quoted names can't hide or fake keywords.
pub fn is_potentially_writing(statement: &str, dialect: Dialect) -> bool {
    writes(&Normalized::new(statement, dialect))
}

fn writes(n: &Normalized) -> bool {
    let kw = n.leading_keyword();
    match kw.as_str() {
        "WITH" => n.contains(&WRITES) || n.contains(&INTO), // data-modifying CTE
        "EXPLAIN" => n.contains(&WRITES) && n.contains(&ANALYZE),
        "SELECT" | "TABLE" | "VALUES" => n.contains(&INTO),
        _ => !READ_ONLY_KEYWORDS.contains(&kw.as_str()),
    }
}

/// Names compare case-insensitively on purpose: stricter than any server, so a rule never misses a table by spelling.
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

pub(crate) fn rule_label(r: &ReadOnlyRule) -> String {
    if r.table.is_empty() {
        format!("Database “{}”", r.database)
    } else {
        format!("“{}” in “{}”", r.table, r.database)
    }
}

/// `schema.table` rules compare only the table name.
pub(crate) fn rule_table_name(r: &ReadOnlyRule) -> &str {
    r.table.rsplit('.').next().unwrap_or(&r.table)
}

/// Port of `ConnectionProfile.checkSQL`: a potentially writing statement is refused on a read-only profile, in a
/// database with a whole-database rule, or when it mentions a protected table as a word outside strings and comments
/// (MySQL also `protected_db.table` from another database). Best effort: triggers, cascades and functions aren't seen;
/// the server's read-only session is the second layer.
pub fn check_sql(profile: &ConnectionProfile, statement: &str, database: &str, dialect: Dialect) -> Result<()> {
    let n = Normalized::new(statement, dialect);
    if !writes(&n) {
        return Ok(());
    }
    if profile.read_only {
        return Err(DbError::ReadOnly);
    }
    // A quoted name is normalized too (`"my-table"` → `my_table`), so both spellings are searched.
    let name = |s: &str| {
        let (plain, normalized) = (fancy_regex::escape(s), fancy_regex::escape(&identifier(s)).into_owned());
        if plain == normalized { format!("(?:{plain})") } else { format!("(?:{plain}|{normalized})") }
    };
    for r in profile.read_only_rules.iter().filter(|r| !r.database.is_empty()) {
        let pattern = if same_name(&r.database, database) {
            if r.table.is_empty() {
                return Err(DbError::ReadOnlyObject(rule_label(r)));
            }
            format!(r"(?i)(?<![\w$]){}(?![\w$])", name(rule_table_name(r)))
        } else if dialect == Dialect::Mysql {
            // MySQL reaches other databases with `db.table` (and `db.*` in GRANT-like statements).
            let table =
                if r.table.is_empty() { String::new() } else { format!(r"{}(?![\w$])", name(rule_table_name(r))) };
            format!(r"(?i)(?<![\w$]){}\s*\.\s*{table}", name(&r.database))
        } else {
            continue;
        };
        // A pattern that can't be built or run refuses (fail closed).
        if Regex::new(&pattern).map_or(true, |re| n.contains(&re)) {
            return Err(DbError::ReadOnlyObject(rule_label(r)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(script: &str, d: Dialect) -> Vec<String> {
        split(script, d).into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn splits_like_gokucore() {
        assert_eq!(texts("select 1; select 2 ;\n\n", Dialect::Postgres), ["select 1", "select 2"]);
        let sql = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT f();";
        assert_eq!(split(sql, Dialect::Postgres).len(), 2);
        let my = "select 'it\\'s; fine';\nDELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END //\nDELIMITER ;\n";
        assert_eq!(texts(my, Dialect::Mysql)[1], "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END");
    }

    #[test]
    fn spans_point_at_the_trimmed_statement() {
        let script = "  -- c\n SELECT 1 ;\n\tSELECT 'é'";
        let chars: Vec<char> = script.chars().collect();
        for st in split(script, Dialect::Postgres) {
            assert_eq!(chars[st.start..st.end].iter().collect::<String>(), st.text);
        }
    }

    #[test]
    fn every_pattern_compiles() {
        for re in [&*USE, &*RETURNING, &*WRITES, &*INTO, &*ANALYZE] {
            assert!(re.is_match("x").is_ok());
        }
        assert!(is_potentially_writing("SELECT * INTO t FROM x", Dialect::Postgres));
        assert!(!is_potentially_writing("SELECT a INTO @v FROM t", Dialect::Mysql));
    }

    #[test]
    fn rules_refuse_the_protected_table_only() {
        let mut p = ConnectionProfile::new("p", crate::profile::DatabaseKind::Mysql);
        p.read_only_rules = vec![ReadOnlyRule { id: "r".into(), database: "shop".into(), table: "orders".into() }];
        assert!(check_sql(&p, "UPDATE orders SET a = 1", "shop", Dialect::Mysql).is_err());
        assert!(check_sql(&p, "UPDATE orders2 SET a = 1", "shop", Dialect::Mysql).is_ok());
        assert!(check_sql(&p, "SELECT * FROM orders", "shop", Dialect::Mysql).is_ok());
        assert!(check_sql(&p, "UPDATE x SET a = 1", "other", Dialect::Mysql).is_ok());
        assert!(check_sql(&p, "UPDATE shop.orders SET a = 1", "other", Dialect::Mysql).is_err());
    }
}
