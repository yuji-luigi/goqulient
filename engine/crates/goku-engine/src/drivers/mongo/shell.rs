//! Port of GokuCore's `MongoShellParser.swift`: mongo-shell input for the query editor. Values are relaxed JSON
//! (unquoted keys, `'`/`"`/backtick strings, comments, trailing commas, `/re/flags`, `ObjectId(…)`, `ISODate(…)` and
//! the other constructors, Extended JSON wrappers); statements are `show dbs|collections`, `use x`, a bare `{…}`
//! command, and `db.coll.method(…)` chains with cursor modifiers. Also the call's classification (`MongoCall.isWrite`,
//! `writeTargets`) and the read-only check (`checkMongo`). Checked against `parity/mongo-shell`. Errors are
//! `DbError::Parse` with the line and column, worded like the app's.
#![warn(clippy::indexing_slicing)]

use crate::error::{DbError, Result};
use crate::profile::ConnectionProfile;
use crate::query::sql::{rule_label, rule_table_name, same_name};
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Document, JavaScriptCodeWithScope, Regex, Timestamp};

/// Deeper values are refused (the app's `maxDepth`).
const MAX_DEPTH: usize = 100;

/// `FindSpec`: a `find` with its cursor modifiers.
#[derive(Debug, Clone, PartialEq)]
pub struct FindSpec {
    pub collection: String,
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub skip: i64,
    pub limit: Option<i64>,
    pub explain: bool,
}

/// One parsed shell statement (`MongoCall`).
#[derive(Debug, Clone, PartialEq)]
pub enum MongoCall {
    ShowDatabases,
    ShowCollections,
    GetName,
    Use(String),
    Command(Document),
    AdminCommand(Document),
    Find(FindSpec),
    Aggregate {
        collection: String,
        pipeline: Vec<Bson>,
    },
    Count {
        collection: String,
        filter: Document,
    },
    Insert {
        collection: String,
        documents: Vec<Document>,
    },
    /// `update` is a document, or an array (an aggregation pipeline update).
    Update {
        collection: String,
        filter: Document,
        update: Bson,
        multi: bool,
        upsert: bool,
    },
    Delete {
        collection: String,
        filter: Document,
        multi: bool,
    },
    RenameCollection {
        from: String,
        to: String,
        drop_target: bool,
    },
}

/// A statement of a script: its text (trimmed), its `char` range in the script and the database it runs in.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellStatement {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub database: String,
    pub call: MongoCall,
}

/// Swift's `as? Document`, which also matches arrays (as documents keyed "0", "1", …).
fn as_doc(v: &Bson) -> Option<Document> {
    match v {
        Bson::Document(d) => Some(d.clone()),
        Bson::Array(a) => Some(a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect()),
        _ => None,
    }
}

/// `doc[key] = value` in Swift's BSON moves the key to the end.
fn set(d: &mut Document, key: &str, value: Bson) {
    d.remove(key);
    d.insert(key, value);
}

fn first_key(d: &Document) -> Option<&str> {
    d.keys().next().map(String::as_str)
}

pub(crate) fn int(v: Option<&Bson>) -> Option<i64> {
    match v? {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) if d.is_finite() => Some(*d as i64),
        _ => None,
    }
}

/// Parses one value (`parseValue`); trailing input is an error.
pub fn parse_value(text: &str) -> Result<Bson> {
    let mut p = Parser::new(text);
    let v = p.value()?;
    p.skip_ws();
    if !p.at_end() {
        return Err(p.error("Unexpected trailing input"));
    }
    Ok(v)
}

/// A filter or sort document (`parseDocument`); empty text is an empty document.
pub fn parse_document(text: &str) -> Result<Document> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Document::new());
    }
    match parse_value(t)? {
        Bson::Document(d) => Ok(d),
        _ => Err(DbError::Parse("Expected a document like { field: value }".into())),
    }
}

/// Every statement with the database it runs in: `use x` switches it for the statements after it.
pub fn parse_script(text: &str, database: &str) -> Result<Vec<ShellStatement>> {
    let mut p = Parser::new(text);
    let mut out = Vec::new();
    let mut db = database.to_owned();
    loop {
        p.skip_ws();
        while p.peek() == Some(';') {
            p.i += 1;
            p.skip_ws();
        }
        if p.at_end() {
            break;
        }
        let start = p.i;
        let call = p.statement()?;
        let range = p.s.get(start..p.i).unwrap_or_default();
        let trail = range.iter().rev().take_while(|c| c.is_whitespace()).count();
        let end = p.i - trail;
        let text: String = p.s.get(start..end).unwrap_or_default().iter().collect();
        let next = if let MongoCall::Use(name) = &call { Some(name.clone()) } else { None };
        out.push(ShellStatement { text, start, end, database: db.clone(), call });
        if let Some(name) = next {
            db = name;
        }
        p.skip_ws();
        if p.peek() == Some(';') {
            p.i += 1;
        }
    }
    Ok(out)
}

struct Parser {
    s: Vec<char>,
    i: usize,
    depth: usize,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_part(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

impl Parser {
    fn new(text: &str) -> Self {
        Self { s: text.chars().collect(), i: 0, depth: 0 }
    }

    fn at_end(&self) -> bool {
        self.i >= self.s.len()
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, k: usize) -> Option<char> {
        self.s.get(k).copied()
    }

    fn error(&self, msg: &str) -> DbError {
        let (mut line, mut col) = (1, 1);
        for &c in self.s.get(..self.i.min(self.s.len())).unwrap_or_default() {
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        DbError::Parse(format!("{msg} at line {line}, column {col}"))
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.i += 1;
            } else if c == '/' && self.at(self.i + 1) == Some('/') {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
            } else if c == '/' && self.at(self.i + 1) == Some('*') {
                self.i += 2;
                while self.i + 1 < self.s.len() && !(self.peek() == Some('*') && self.at(self.i + 1) == Some('/')) {
                    self.i += 1;
                }
                self.i = self.s.len().min(self.i + 2);
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: char) -> Result<()> {
        self.skip_ws();
        if self.peek() != Some(c) {
            return Err(self.error(&format!("Expected '{c}'")));
        }
        self.i += 1;
        Ok(())
    }

    fn identifier(&mut self) -> Option<String> {
        self.skip_ws();
        if !self.peek().is_some_and(is_ident_start) {
            return None;
        }
        let start = self.i;
        while self.peek().is_some_and(is_ident_part) {
            self.i += 1;
        }
        Some(self.slice(start, self.i))
    }

    fn slice(&self, a: usize, b: usize) -> String {
        self.s.get(a..b).unwrap_or_default().iter().collect()
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let digits = self.s.get(at..at + 4)?.iter().collect::<String>();
        u32::from_str_radix(&digits, 16).ok()
    }

    fn string(&mut self) -> Result<String> {
        self.skip_ws();
        let Some(q) = self.peek().filter(|q| matches!(q, '"' | '\'' | '`')) else {
            return Err(self.error("Expected a string"));
        };
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else { return Err(self.error("Unterminated string")) };
            self.i += 1;
            if c == q {
                break;
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(e) = self.peek() else { return Err(self.error("Unterminated string")) };
            self.i += 1;
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                '0' => out.push('\0'),
                'u' => {
                    let Some(v) = self.hex4(self.i) else { return Err(self.error("Bad \\u escape")) };
                    self.i += 4;
                    let low = (self.peek() == Some('\\') && self.at(self.i + 1) == Some('u'))
                        .then(|| self.hex4(self.i + 2))
                        .flatten()
                        .filter(|l| (0xDC00..=0xDFFF).contains(l));
                    if let (true, Some(low)) = ((0xD800..=0xDBFF).contains(&v), low) {
                        self.i += 6;
                        out.extend(char::from_u32(0x10000 + ((v - 0xD800) << 10) + (low - 0xDC00)));
                    } else {
                        out.extend(char::from_u32(v)); // a lone surrogate is dropped
                    }
                }
                other => out.push(other),
            }
        }
        Ok(out)
    }

    fn value(&mut self) -> Result<Bson> {
        self.depth += 1;
        let v = self.value_inner();
        self.depth -= 1;
        v
    }

    fn value_inner(&mut self) -> Result<Bson> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error("Nesting too deep"));
        }
        self.skip_ws();
        let Some(c) = self.peek() else { return Err(self.error("Expected a value")) };
        match c {
            '{' => self.object(),
            '[' => self.array(),
            '"' | '\'' | '`' => self.string().map(Bson::String),
            '/' => self.regex(),
            _ if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() => self.number(),
            _ => match self.identifier() {
                Some(id) => self.identifier_value(id),
                None => Err(self.error(&format!("Unexpected character '{c}'"))),
            },
        }
    }

    fn number(&mut self) -> Result<Bson> {
        let start = self.i;
        if matches!(self.peek(), Some('-' | '+')) {
            self.i += 1;
        }
        if self.identifier().as_deref() == Some("Infinity") {
            return Ok(Bson::Double(if self.at(start) == Some('-') { f64::NEG_INFINITY } else { f64::INFINITY }));
        }
        let mut is_float = false;
        while let Some(c) = self.peek() {
            let exponent_sign =
                (c == '-' || c == '+') && matches!(self.i.checked_sub(1).and_then(|k| self.at(k)), Some('e' | 'E'));
            if !(c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || exponent_sign) {
                break;
            }
            is_float |= c == '.' || c == 'e' || c == 'E';
            self.i += 1;
        }
        let text = self.slice(start, self.i);
        if !is_float && let Ok(v) = text.parse::<i64>() {
            return Ok(i32::try_from(v).map_or(Bson::Int64(v), Bson::Int32));
        }
        swift_double(&text).map(Bson::Double).ok_or_else(|| self.error(&format!("Invalid number '{text}'")))
    }

    fn regex(&mut self) -> Result<Bson> {
        self.i += 1;
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            let Some(c) = self.peek() else { return Err(self.error("Unterminated regular expression")) };
            self.i += 1;
            if c == '\\'
                && let Some(n) = self.peek()
            {
                pattern.push(c);
                pattern.push(n);
                self.i += 1;
                continue;
            }
            if c == '[' {
                in_class = true;
            } else if c == ']' {
                in_class = false;
            }
            if c == '/' && !in_class {
                break;
            }
            pattern.push(c);
        }
        let mut flags: Vec<char> = Vec::new();
        while let Some(c) = self.peek().filter(|c| c.is_alphabetic()) {
            flags.push(c);
            self.i += 1;
        }
        flags.sort_unstable();
        regex(pattern, flags.into_iter().collect()).ok_or_else(|| self.error("Invalid regular expression"))
    }

    fn arguments(&mut self) -> Result<Vec<Bson>> {
        self.expect('(')?;
        let mut args = Vec::new();
        self.skip_ws();
        if self.peek() == Some(')') {
            self.i += 1;
            return Ok(args);
        }
        loop {
            args.push(self.value()?);
            self.skip_ws();
            if self.peek() == Some(',') {
                self.i += 1;
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.i += 1;
                    return Ok(args);
                }
                continue;
            }
            self.expect(')')?;
            return Ok(args);
        }
    }

    fn identifier_value(&mut self, id: String) -> Result<Bson> {
        let id = match id.as_str() {
            "true" => return Ok(Bson::Boolean(true)),
            "false" => return Ok(Bson::Boolean(false)),
            "null" | "undefined" => return Ok(Bson::Null),
            "NaN" => return Ok(Bson::Double(f64::NAN)),
            "Infinity" => return Ok(Bson::Double(f64::INFINITY)),
            "new" => self.identifier().ok_or_else(|| self.error("Expected constructor after 'new'"))?,
            _ => id,
        };
        let args = self.arguments()?;
        let str_arg = |p: &Self, k: usize| -> Result<String> {
            match args.get(k) {
                None => Err(p.error(&format!("{id}() needs an argument"))),
                Some(Bson::String(s)) => Ok(s.clone()),
                Some(Bson::Int32(n)) => Ok(n.to_string()),
                Some(Bson::Int64(n)) => Ok(n.to_string()),
                Some(Bson::Double(d)) => Ok(super::format::double(*d)),
                Some(_) => Err(p.error(&format!("{id}() expects a string"))),
            }
        };
        let fail = |p: &Self, msg: &str| p.error(msg);
        Ok(match id.as_str() {
            "ObjectId" => {
                if args.is_empty() {
                    return Ok(Bson::ObjectId(bson::oid::ObjectId::new()));
                }
                Bson::ObjectId(object_id(&str_arg(self, 0)?).ok_or_else(|| fail(self, "Invalid ObjectId"))?)
            }
            "ISODate" | "Date" => match args.first() {
                None => Bson::DateTime(DateTime::now()),
                Some(Bson::Int64(ms)) => Bson::DateTime(millis(*ms as f64)),
                Some(Bson::Int32(ms)) => Bson::DateTime(millis(f64::from(*ms))),
                Some(Bson::Double(ms)) => Bson::DateTime(millis(*ms)),
                Some(_) => Bson::DateTime(parse_date(&str_arg(self, 0)?).ok_or_else(|| fail(self, "Invalid date"))?),
            },
            "NumberLong" | "Long" => {
                Bson::Int64(str_arg(self, 0)?.parse().map_err(|_| fail(self, "Invalid NumberLong"))?)
            }
            "NumberInt" | "Int32" => {
                Bson::Int32(str_arg(self, 0)?.parse().map_err(|_| fail(self, "Invalid NumberInt"))?)
            }
            "NumberDecimal" | "Decimal128" => {
                Bson::Decimal128(decimal(&str_arg(self, 0)?).ok_or_else(|| fail(self, "Invalid NumberDecimal"))?)
            }
            "Double" => Bson::Double(swift_double(&str_arg(self, 0)?).ok_or_else(|| fail(self, "Invalid Double"))?),
            "UUID" => uuid(&str_arg(self, 0)?).ok_or_else(|| fail(self, "Invalid UUID"))?,
            "BinData" => {
                let data = if args.len() == 2 { base64(&str_arg(self, 1)?) } else { None };
                // The app always stores subtype 0, whatever the first argument says.
                let bytes = data.ok_or_else(|| fail(self, "BinData(subtype, base64) expected"))?;
                Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes })
            }
            "Timestamp" => {
                let t = if args.len() == 2 { str_arg(self, 0)?.parse::<i32>().ok() } else { None };
                let parsed = match t {
                    Some(t) => str_arg(self, 1)?.parse::<i32>().ok().map(|inc| (t, inc)),
                    None => None,
                };
                let (t, inc) = parsed.ok_or_else(|| fail(self, "Timestamp(t, i) expected"))?;
                Bson::Timestamp(Timestamp { time: t as u32, increment: inc as u32 })
            }
            "RegExp" => {
                let options = if args.len() > 1 { str_arg(self, 1)? } else { String::new() };
                regex(str_arg(self, 0)?, options).ok_or_else(|| fail(self, "Invalid regular expression"))?
            }
            "MinKey" => Bson::MinKey,
            "MaxKey" => Bson::MaxKey,
            _ => return Err(self.error(&format!("Unknown function {id}()"))),
        })
    }

    fn object(&mut self) -> Result<Bson> {
        self.expect('{')?;
        let mut doc = Document::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(Bson::Document(doc));
        }
        loop {
            self.skip_ws();
            let key = if matches!(self.peek(), Some('"' | '\'' | '`')) {
                self.string()?
            } else {
                // Unquoted keys may contain dots ("a.b": 1 in filters) and digits.
                let start = self.i;
                while self.peek().is_some_and(|c| is_ident_part(c) || c == '.') {
                    self.i += 1;
                }
                if self.i == start {
                    return Err(self.error("Expected a field name"));
                }
                self.slice(start, self.i)
            };
            self.expect(':')?;
            let v = self.value()?;
            set(&mut doc, &key, v);
            self.skip_ws();
            if self.peek() == Some(',') {
                self.i += 1;
                self.skip_ws();
                if self.peek() == Some('}') {
                    self.i += 1;
                    break;
                }
                continue;
            }
            self.expect('}')?;
            break;
        }
        Ok(extended_json(&doc).unwrap_or(Bson::Document(doc)))
    }

    fn array(&mut self) -> Result<Bson> {
        self.expect('[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(Bson::Array(out));
        }
        loop {
            out.push(self.value()?);
            self.skip_ws();
            if self.peek() == Some(',') {
                self.i += 1;
                self.skip_ws();
                if self.peek() == Some(']') {
                    self.i += 1;
                    break;
                }
                continue;
            }
            self.expect(']')?;
            break;
        }
        Ok(Bson::Array(out))
    }

    // MARK: Statements

    fn statement(&mut self) -> Result<MongoCall> {
        self.skip_ws();
        let save = self.i;
        if let Some(word) = self.identifier() {
            match word.as_str() {
                "show" => {
                    let Some(what) = self.identifier() else {
                        return Err(self.error("show what? (dbs, collections)"));
                    };
                    return match what.as_str() {
                        "dbs" | "databases" => Ok(MongoCall::ShowDatabases),
                        "collections" | "tables" => Ok(MongoCall::ShowCollections),
                        _ => Err(self.error(&format!("Unsupported: show {what}"))),
                    };
                }
                "use" => {
                    self.skip_ws();
                    let start = self.i;
                    while self.peek().is_some_and(|c| !c.is_whitespace() && c != ';') {
                        self.i += 1;
                    }
                    return Ok(MongoCall::Use(self.slice(start, self.i)));
                }
                "db" => return self.db_chain(),
                _ => self.i = save,
            }
        }
        // A bare document is run as a database command.
        match self.value()? {
            Bson::Document(d) => Ok(MongoCall::Command(d)),
            _ => Err(self.error("Expected db.<collection>.<method>(...), show/use, or a command document")),
        }
    }

    fn db_chain(&mut self) -> Result<MongoCall> {
        let mut path: Vec<String> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some('.') => {
                    self.i += 1;
                    let Some(name) = self.identifier() else { return Err(self.error("Expected a name after '.'")) };
                    self.skip_ws();
                    if self.peek() == Some('(') {
                        let args = self.arguments()?;
                        return self.method_call(&path, &name, args);
                    }
                    path.push(name);
                }
                Some('[') => {
                    self.i += 1;
                    path.push(self.string()?);
                    self.expect(']')?;
                }
                _ => return Err(self.error("Incomplete statement: expected a method call such as .find()")),
            }
        }
    }

    fn method_call(&mut self, path: &[String], method: &str, args: Vec<Bson>) -> Result<MongoCall> {
        if path.is_empty() {
            return self.db_method(method, args);
        }
        let coll = path.join(".");
        let doc = |p: &Self, k: usize| -> Result<Document> {
            match args.get(k) {
                None => Ok(Document::new()),
                Some(Bson::Document(d)) => Ok(d.clone()),
                Some(_) => Err(p.error(&format!("{method}() argument {} must be a document", k + 1))),
            }
        };
        let mut op = match method {
            "find" | "findOne" => MongoCall::Find(FindSpec {
                collection: coll,
                filter: doc(self, 0)?,
                projection: if args.len() > 1 { Some(doc(self, 1)?) } else { None },
                sort: None,
                skip: 0,
                limit: (method == "findOne").then_some(1),
                explain: false,
            }),
            "aggregate" => match args.first() {
                Some(Bson::Array(p)) => MongoCall::Aggregate { collection: coll, pipeline: p.clone() },
                _ => return Err(self.error("aggregate() expects an array pipeline")),
            },
            "countDocuments" | "count" => MongoCall::Count { collection: coll, filter: doc(self, 0)? },
            "estimatedDocumentCount" => MongoCall::Command(bson::doc! {"count": coll}),
            "distinct" => {
                let Some(Bson::String(key)) = args.first() else {
                    return Err(self.error("distinct(field) expects a string"));
                };
                let query = if args.len() > 1 { doc(self, 1)? } else { Document::new() };
                MongoCall::Command(bson::doc! {"distinct": coll, "key": key.clone(), "query": query})
            }
            "insertOne" | "insert" => match args.first() {
                Some(Bson::Array(a)) => {
                    MongoCall::Insert { collection: coll, documents: a.iter().filter_map(as_doc).collect() }
                }
                _ => MongoCall::Insert { collection: coll, documents: vec![doc(self, 0)?] },
            },
            "insertMany" => match args.first() {
                Some(Bson::Array(a)) => {
                    MongoCall::Insert { collection: coll, documents: a.iter().filter_map(as_doc).collect() }
                }
                _ => return Err(self.error("insertMany() expects an array")),
            },
            "updateOne" | "updateMany" | "replaceOne" => {
                let update = match args.get(1) {
                    Some(u @ (Bson::Document(_) | Bson::Array(_))) if args.len() >= 2 => u.clone(),
                    _ => return Err(self.error(&format!("{method}(filter, update) expects two arguments"))),
                };
                let opts = if args.len() > 2 { doc(self, 2)? } else { Document::new() };
                MongoCall::Update {
                    filter: doc(self, 0)?,
                    collection: coll,
                    update,
                    multi: method == "updateMany",
                    upsert: matches!(opts.get("upsert"), Some(Bson::Boolean(true))),
                }
            }
            "deleteOne" | "deleteMany" | "remove" => {
                MongoCall::Delete { collection: coll, filter: doc(self, 0)?, multi: method != "deleteOne" }
            }
            "getIndexes" => MongoCall::Command(bson::doc! {"listIndexes": coll}),
            "createIndex" => {
                let mut index = doc(self, 1)?;
                let keys = doc(self, 0)?;
                if !index.contains_key("name") {
                    let name: Vec<String> = keys
                        .iter()
                        .map(|(k, v)| format!("{k}_{}", super::format::cell(Some(v)).unwrap_or_default()))
                        .collect();
                    set(&mut index, "name", Bson::String(name.join("_")));
                }
                set(&mut index, "key", Bson::Document(keys));
                MongoCall::Command(bson::doc! {"createIndexes": coll, "indexes": [index]})
            }
            "dropIndex" => {
                let index = args.first().cloned().unwrap_or_else(|| Bson::String("*".into()));
                MongoCall::Command(bson::doc! {"dropIndexes": coll, "index": index})
            }
            "drop" => MongoCall::Command(bson::doc! {"drop": coll}),
            "stats" => MongoCall::Command(bson::doc! {"collStats": coll}),
            "renameCollection" => {
                let Some(Bson::String(to)) = args.first() else {
                    return Err(self.error("renameCollection(newName) expects a string"));
                };
                let drop_target = matches!(args.get(1), Some(Bson::Boolean(true)));
                MongoCall::RenameCollection { from: coll, to: to.clone(), drop_target }
            }
            _ => return Err(self.error(&format!("Unsupported collection method {method}()"))),
        };
        // Cursor modifiers: .sort() .limit() .skip() .projection() .count() .toArray() .pretty() .explain()
        loop {
            self.skip_ws();
            if self.peek() != Some('.') {
                break;
            }
            self.i += 1;
            let Some(m) = self.identifier() else { return Err(self.error("Expected a cursor method")) };
            let a = self.arguments()?;
            let MongoCall::Find(f) = &mut op else {
                if m == "toArray" || m == "pretty" {
                    continue;
                }
                return Err(self.error(&format!(".{m}() is only supported after find()")));
            };
            let number = |v: Option<&Bson>| match v {
                Some(Bson::Int32(n)) => Some(i64::from(*n)),
                Some(Bson::Int64(n)) => Some(*n),
                _ => None,
            };
            match m.as_str() {
                "sort" => f.sort = a.first().and_then(as_doc),
                "limit" => f.limit = number(a.first()).or(f.limit),
                "skip" => f.skip = number(a.first()).unwrap_or(0),
                "projection" => f.projection = a.first().and_then(as_doc),
                "toArray" | "pretty" => {}
                "count" | "itcount" | "size" => {
                    op = MongoCall::Count { collection: f.collection.clone(), filter: f.filter.clone() };
                }
                "explain" => f.explain = true,
                _ => return Err(self.error(&format!("Unsupported cursor method .{m}()"))),
            }
        }
        Ok(op)
    }

    fn db_method(&mut self, method: &str, args: Vec<Bson>) -> Result<MongoCall> {
        Ok(match method {
            "getCollection" => {
                let Some(Bson::String(name)) = args.first() else {
                    return Err(self.error("getCollection(name) expects a string"));
                };
                self.skip_ws();
                if self.peek() != Some('.') {
                    return Err(self.error("Expected a method after getCollection()"));
                }
                self.i += 1;
                let Some(m) = self.identifier() else { return Err(self.error("Expected a method name")) };
                let args = self.arguments()?;
                return self.method_call(std::slice::from_ref(name), &m, args);
            }
            "runCommand" | "adminCommand" => {
                let Some(d) = args.first().and_then(as_doc) else {
                    return Err(self.error(&format!("{method} expects a document")));
                };
                if method == "adminCommand" { MongoCall::AdminCommand(d) } else { MongoCall::Command(d) }
            }
            "getCollectionNames" | "getCollectionInfos" => MongoCall::ShowCollections,
            "createCollection" => {
                let Some(Bson::String(n)) = args.first() else {
                    return Err(self.error("createCollection(name) expects a string"));
                };
                let mut d = bson::doc! {"create": n.clone()};
                if let Some(opts) = args.get(1).and_then(as_doc) {
                    for (k, v) in opts {
                        set(&mut d, &k, v);
                    }
                }
                MongoCall::Command(d)
            }
            "dropDatabase" => MongoCall::Command(bson::doc! {"dropDatabase": 1}),
            "stats" => MongoCall::Command(bson::doc! {"dbStats": 1}),
            "serverStatus" => MongoCall::AdminCommand(bson::doc! {"serverStatus": 1}),
            "hello" | "isMaster" => MongoCall::AdminCommand(bson::doc! {"hello": 1}),
            "version" => MongoCall::AdminCommand(bson::doc! {"buildInfo": 1}),
            "getName" => MongoCall::GetName,
            _ => return Err(self.error(&format!("Unsupported db method {method}()"))),
        })
    }
}

// MARK: - Scalars

/// Swift's `Double(String)`: decimal floats, `inf`/`infinity`/`nan` in any case, with an optional sign.
fn swift_double(text: &str) -> Option<f64> {
    if text.is_empty() || text.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    text.parse().ok()
}

/// Seconds from 1970 to Foundation's reference date (2001-01-01), which `Date` counts from.
const REFERENCE_DATE: f64 = 978_307_200.0;

/// A date from `ms` milliseconds the way the app stores it: `Date(timeIntervalSince1970: ms / 1000)` (kept from the
/// reference date), then BSON's `Int(date.timeIntervalSince1970 * 1000)`, which truncates. The same double operations
/// give the same milliseconds (`ISODate(1.5)` is 1 ms).
fn millis(ms: f64) -> DateTime {
    let since_reference = ms / 1000.0 - REFERENCE_DATE;
    DateTime::from_millis(((since_reference + REFERENCE_DATE) * 1000.0).trunc() as i64)
}

fn object_id(s: &str) -> Option<bson::oid::ObjectId> {
    (s.len() == 24).then(|| bson::oid::ObjectId::parse_str(s).ok()).flatten()
}

fn regex(pattern: String, options: String) -> Option<Bson> {
    Some(Bson::RegularExpression(Regex { pattern: pattern.try_into().ok()?, options: options.try_into().ok()? }))
}

/// `UUID(uuidString:)`: exactly 8-4-4-4-12 hex digits, any case; stored as binary subtype 4.
fn uuid(s: &str) -> Option<Bson> {
    let groups: Vec<&str> = s.split('-').collect();
    if groups.iter().map(|g| g.len()).collect::<Vec<_>>() != [8, 4, 4, 4, 12] {
        return None;
    }
    let hex: String = groups.concat();
    let bytes = (0..16)
        .map(|k| hex.get(k * 2..k * 2 + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect::<Option<Vec<u8>>>()?;
    Some(Bson::Binary(Binary { subtype: BinarySubtype::Uuid, bytes }))
}

/// Foundation's `Data(base64Encoded:)`: the standard alphabet with padding, no whitespace.
fn base64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// `parseDate` as the app (Linux Foundation) behaves: an RFC 3339 date-time with optional fraction and a `Z` or
/// `±hh:mm` zone; otherwise a leading `yyyy-MM-dd` is midnight UTC and whatever follows is ignored.
pub(crate) fn parse_date(s: &str) -> Option<DateTime> {
    let t = s.trim_matches(|c: char| c.is_whitespace() && !matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}'));
    let b = t.as_bytes();
    let num = |a: usize, n: usize| -> Option<i64> {
        let part = b.get(a..a + n)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0i64, |v, d| v * 10 + i64::from(d - b'0')))
    };
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || !(1..=12).contains(&month) {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if day < 1 || day > *days_in_month.get(usize::try_from(month - 1).ok()?)? {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let midnight = days * 86_400_000;
    let time = (|| {
        if b.get(10) != Some(&b'T') || b.get(13) != Some(&b':') || b.get(16) != Some(&b':') {
            return None;
        }
        let (h, m, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
        if h > 23 || m > 59 || sec > 59 {
            return None;
        }
        let mut k = 19;
        let mut fraction_ms = 0.0;
        if b.get(k) == Some(&b'.') {
            let digits = b.get(k + 1..)?.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits == 0 {
                return None;
            }
            fraction_ms = t.get(k..k + 1 + digits)?.parse::<f64>().ok()? * 1000.0;
            k += 1 + digits;
        }
        let offset_minutes = match b.get(k..)? {
            [b'Z'] => 0,
            [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
                let (oh, om) = (num(k + 1, 2)?, num(k + 4, 2)?);
                (oh * 60 + om) * if *sign == b'-' { -1 } else { 1 }
            }
            _ => return None,
        };
        Some((h * 3600 + m * 60 + sec - offset_minutes * 60) * 1000 + fraction_ms.round() as i64)
    })();
    Some(millis((midnight + time.unwrap_or(0)) as f64))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Port of `Decimal128Codec.parse`: up to 34 digits, exponent in range, `NaN` and `Inf`/`Infinity`; no rounding.
pub(crate) fn decimal(text: &str) -> Option<Decimal128> {
    let mut s = text.trim_matches(|c: char| c == ' ' || c == '\t');
    let mut negative = false;
    if let Some(rest) = s.strip_prefix('-') {
        negative = true;
        s = rest;
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    }
    let make = |low: u64, high: u64| {
        let mut bytes = [0u8; 16];
        let (lo, hi) = bytes.split_at_mut(8);
        lo.copy_from_slice(&low.to_le_bytes());
        hi.copy_from_slice(&high.to_le_bytes());
        Decimal128::from_bytes(bytes)
    };
    let sign = if negative { 1u64 << 63 } else { 0 };
    match s.to_lowercase().as_str() {
        "nan" => return Some(make(0, 0x7C00_0000_0000_0000)),
        "inf" | "infinity" => return Some(make(0, sign | 0x7800_0000_0000_0000)),
        _ => {}
    }
    let mut exponent: i64 = 0;
    if let Some(e) = s.find(['e', 'E']) {
        exponent = s.get(e + 1..)?.parse().ok()?;
        s = s.get(..e)?;
    }
    let mut digits = String::new();
    let mut seen_point = false;
    for ch in s.chars() {
        if ch == '.' {
            if seen_point {
                return None;
            }
            seen_point = true;
            continue;
        }
        if !ch.is_ascii_digit() {
            return None;
        }
        digits.push(ch);
        if seen_point {
            exponent -= 1;
        }
    }
    if digits.is_empty() {
        return None;
    }
    let digits = match digits.trim_start_matches('0') {
        "" => "0",
        d => d,
    };
    if digits.len() > 34 || !(-6176..=6111).contains(&exponent) {
        return None;
    }
    let coefficient = digits.chars().fold(0u128, |v, d| v * 10 + u128::from(d as u8 - b'0'));
    let (hi, lo) = ((coefficient >> 64) as u64, coefficient as u64);
    let high = sign | (((exponent + 6176) as u64) << 49) | (hi & 0x1_FFFF_FFFF_FFFF);
    Some(make(lo, high))
}

// MARK: - Extended JSON wrappers

fn uint32(v: Option<&Bson>) -> Option<u32> {
    match v? {
        Bson::Int32(i) => Some(*i as u32),
        Bson::Int64(i) => u32::try_from(*i).ok(),
        _ => None,
    }
}

/// Canonical / relaxed Extended JSON wrappers (`{$oid: …}`, `{$date: …}`, …); `None` leaves the document as it is.
fn extended_json(d: &Document) -> Option<Bson> {
    if d.len() == 2
        && let (Some(Bson::String(code)), Some(scope)) = (d.get("$code"), d.get("$scope").and_then(as_doc))
    {
        return Some(Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope { code: code.clone(), scope }));
    }
    let (k, v) = d.iter().next().filter(|(k, _)| d.len() == 1 && k.starts_with('$'))?;
    let string = || if let Bson::String(s) = v { Some(s.as_str()) } else { None };
    match k.as_str() {
        "$oid" => object_id(string()?).map(Bson::ObjectId),
        "$date" => match v {
            Bson::String(s) => parse_date(s).map(Bson::DateTime),
            Bson::Int64(n) => Some(Bson::DateTime(millis(*n as f64))),
            Bson::Int32(n) => Some(Bson::DateTime(millis(f64::from(*n)))),
            _ => None,
        },
        "$numberLong" => string()?.parse().ok().map(Bson::Int64),
        "$numberInt" => string()?.parse().ok().map(Bson::Int32),
        "$numberDouble" => swift_double(string()?).map(Bson::Double),
        "$numberDecimal" => decimal(string()?).map(Bson::Decimal128),
        "$uuid" => uuid(string()?),
        "$binary" => {
            let b = as_doc(v)?;
            let bytes = base64(b.get_str("base64").ok()?)?;
            let subtype = u8::from_str_radix(b.get_str("subType").ok()?, 16).ok()?;
            Some(Bson::Binary(Binary { subtype: BinarySubtype::from(subtype), bytes }))
        }
        "$regularExpression" => {
            let r = as_doc(v)?;
            regex(r.get_str("pattern").ok()?.to_owned(), r.get_str("options").unwrap_or_default().to_owned())
        }
        "$timestamp" => {
            let t = as_doc(v)?;
            Some(Bson::Timestamp(Timestamp { time: uint32(t.get("t"))?, increment: uint32(t.get("i"))? }))
        }
        "$minKey" => Some(Bson::MinKey),
        "$maxKey" => Some(Bson::MaxKey),
        "$code" => Some(Bson::JavaScriptCode(string()?.to_owned())),
        _ => None,
    }
}

// MARK: - Classification

/// Commands that only read, including what the shell helpers send (`stats()`, `getIndexes()`, `version()`, …): the
/// app's `MongoCall.readCommands`. Every other command is a write (an allowlist, so rare or aliased write commands are
/// never missed); reads that can show other clients' secrets (`usersInfo`, `getLog`, `currentOp`) are left out on purpose.
pub const READ_COMMANDS: &[&str] = &[
    "find",
    "aggregate",
    "count",
    "distinct",
    "getMore",
    "explain",
    "listCollections",
    "listIndexes",
    "listDatabases",
    "listSearchIndexes",
    "dbStats",
    "collStats",
    "dataSize",
    "dbHash",
    "serverStatus",
    "hello",
    "isMaster",
    "ismaster",
    "buildInfo",
    "buildinfo",
    "ping",
    "connectionStatus",
    "hostInfo",
    "whatsmyuri",
    "getParameter",
    "top",
    "lockInfo",
    "replSetGetStatus",
    "replSetGetConfig",
    "rolesInfo",
];
/// Write commands whose first value names the collection they change.
const COLLECTION_WRITE_COMMANDS: &[&str] = &[
    "insert",
    "update",
    "delete",
    "findAndModify",
    "findandmodify",
    "create",
    "drop",
    "createIndexes",
    "dropIndexes",
    "collMod",
    "compact",
    "convertToCapped",
    "emptycapped",
    "reIndex",
    "validate",
];
/// Write commands that change the database as a whole (users, roles, the database itself).
const DATABASE_WRITE_COMMANDS: &[&str] = &[
    "dropDatabase",
    "createUser",
    "dropUser",
    "updateUser",
    "grantRolesToUser",
    "revokeRolesFromUser",
    "dropAllUsersFromDatabase",
    "createRole",
    "dropRole",
    "updateRole",
    "grantPrivilegesToRole",
    "revokePrivilegesFromRole",
    "grantRolesToRole",
    "revokeRolesFromRole",
    "dropAllRolesFromDatabase",
];

/// The stages of a pipeline given as an array or (Swift's `as? Document`) a document.
fn stages(pipeline: Option<&Bson>) -> Vec<Document> {
    pipeline.and_then(as_doc).map(|p| p.into_iter().filter_map(|(_, s)| as_doc(&s)).collect()).unwrap_or_default()
}

/// An aggregation pipeline writes when a stage is `$out` or `$merge`.
fn pipeline_writes(stages: &[Document]) -> bool {
    stages.iter().any(|s| s.contains_key("$out") || s.contains_key("$merge"))
}

/// `MongoCall.commandWrites`: the first key names the command.
pub fn command_writes(d: &Document) -> bool {
    let Some((key, value)) = d.iter().next() else { return false };
    match key.as_str() {
        "aggregate" => pipeline_writes(&stages(d.get("pipeline"))),
        "explain" => as_doc(value).is_none_or(|d| command_writes(&d)),
        k => !READ_COMMANDS.contains(&k),
    }
}

impl MongoCall {
    /// Whether the call can modify data (read-only mode and rules).
    pub fn is_write(&self) -> bool {
        match self {
            Self::ShowDatabases | Self::ShowCollections | Self::GetName | Self::Use(_) | Self::Find(_) => false,
            Self::Count { .. } => false,
            Self::Insert { .. } | Self::Update { .. } | Self::Delete { .. } | Self::RenameCollection { .. } => true,
            Self::Aggregate { pipeline, .. } => pipeline_writes(&stages(Some(&Bson::Array(pipeline.clone())))),
            Self::Command(d) | Self::AdminCommand(d) => command_writes(d),
        }
    }

    /// The database the call acts on when the script's current database is `db` (admin commands act on `admin`).
    pub fn database<'a>(&self, db: &'a str) -> &'a str {
        if matches!(self, Self::AdminCommand(_)) { "admin" } else { db }
    }

    /// (database, collection) pairs a write call modifies; collection `None` = the database as a whole. `None` when
    /// the target can't be told (`applyOps`, `bulkWrite`, any command not known to target one collection), which
    /// read-only rules treat as touching everything.
    pub fn write_targets(&self, db: &str) -> Option<Vec<(String, Option<String>)>> {
        fn ns(value: Option<&Bson>, fallback: &str) -> Option<(String, Option<String>)> {
            match value? {
                Bson::String(s) => Some((fallback.to_owned(), Some(s.clone()))),
                v => {
                    let d = as_doc(v)?;
                    let db = d.get_str("db").unwrap_or(fallback).to_owned();
                    if let Some(into) = d.get("into") {
                        return ns(Some(into), &db);
                    }
                    Some((db, Some(d.get_str("coll").ok()?.to_owned())))
                }
            }
        }
        fn namespace(value: Option<&Bson>) -> Option<(String, Option<String>)> {
            let (db, coll) = value?.as_str()?.split_once('.')?;
            Some((db.to_owned(), Some(coll.to_owned())))
        }
        fn pipeline_targets(stages: &[Document], db: &str) -> Option<Vec<(String, Option<String>)>> {
            let mut out = Vec::new();
            for stage in stages {
                for key in ["$out", "$merge"] {
                    if stage.contains_key(key) {
                        out.push(ns(stage.get(key), db)?);
                    }
                }
            }
            Some(out)
        }
        let string = |d: &Document, k: &str| d.get_str(k).ok().map(str::to_owned);
        match self {
            Self::Insert { collection, .. } | Self::Update { collection, .. } | Self::Delete { collection, .. } => {
                Some(vec![(db.to_owned(), Some(collection.clone()))])
            }
            Self::RenameCollection { from, to, .. } => {
                Some(vec![(db.to_owned(), Some(from.clone())), (db.to_owned(), Some(to.clone()))])
            }
            Self::Aggregate { pipeline, .. } => pipeline_targets(&stages(Some(&Bson::Array(pipeline.clone()))), db),
            Self::Command(d) | Self::AdminCommand(d) => {
                let target = self.database(db);
                let Some(key) = first_key(d).filter(|_| command_writes(d)) else { return Some(Vec::new()) };
                match key {
                    "aggregate" => pipeline_targets(&stages(d.get("pipeline")), target),
                    "renameCollection" => Some(vec![namespace(d.get("renameCollection"))?, namespace(d.get("to"))?]),
                    "cloneCollectionAsCapped" => Some(vec![
                        (target.to_owned(), Some(string(d, key)?)),
                        (target.to_owned(), Some(string(d, "toCollection")?)),
                    ]),
                    "mapReduce" => {
                        // `out: 'x'` or `{ replace | merge | reduce: 'x', db: 'y' }`; `{ inline: 1 }` writes nothing.
                        if let Some(out) = string(d, "out") {
                            return Some(vec![(target.to_owned(), Some(out))]);
                        }
                        let out = as_doc(d.get("out")?)?;
                        if out.contains_key("inline") {
                            return Some(Vec::new());
                        }
                        let coll = ["replace", "merge", "reduce"].iter().find_map(|k| out.get(*k))?.as_str()?;
                        Some(vec![(string(&out, "db").unwrap_or_else(|| target.to_owned()), Some(coll.to_owned()))])
                    }
                    k if DATABASE_WRITE_COMMANDS.contains(&k) => Some(vec![(target.to_owned(), None)]),
                    k if COLLECTION_WRITE_COMMANDS.contains(&k) => Some(vec![(target.to_owned(), Some(string(d, k)?))]),
                    _ => None,
                }
            }
            _ => Some(Vec::new()),
        }
    }
}

/// Port of `ConnectionProfile.checkMongo`: read-only mode plus the per-database / per-collection rules. MongoDB has no
/// server-side read-only, so this check is the only one and stays strict: a write whose target can't be told is
/// refused as soon as any rule exists.
pub fn check_mongo(profile: &ConnectionProfile, call: &MongoCall, database: &str) -> Result<()> {
    if !call.is_write() {
        return Ok(());
    }
    if profile.read_only {
        return Err(DbError::ReadOnly);
    }
    let rules = profile.read_only_rules.iter().filter(|r| !r.database.is_empty());
    let Some(targets) = call.write_targets(database) else {
        return match rules.into_iter().next() {
            Some(r) => Err(DbError::ReadOnlyObject(rule_label(r))),
            None => Ok(()),
        };
    };
    for (db, table) in targets {
        for r in rules.clone().filter(|r| same_name(&r.database, &db)) {
            if r.table.is_empty() || table.as_deref().is_none_or(|t| same_name(rule_table_name(r), t)) {
                return Err(DbError::ReadOnlyObject(rule_label(r)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_find_chain() {
        let s = parse_script("use x; db.orders.find({ a: 1 }).sort({ b: -1 }).limit(5)", "shop").unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].database, "x");
        let MongoCall::Find(f) = &s[1].call else { panic!("find expected") };
        assert_eq!((f.limit, f.sort.clone()), (Some(5), Some(bson::doc! {"b": -1})));
        assert_eq!(&s[1].text, "db.orders.find({ a: 1 }).sort({ b: -1 }).limit(5)");
    }

    #[test]
    fn writes_are_classified() {
        let w = |s: &str| parse_script(s, "shop").unwrap()[0].call.is_write();
        assert!(!w("db.x.find()") && !w("db.x.aggregate([{ $match: {} }])") && !w("{ ping: 1 }"));
        assert!(w("db.x.aggregate([{ $out: 'y' }])") && w("db.x.insertOne({})") && w("{ usersInfo: 1 }"));
        assert!(w("{ explain: { delete: 'x', deletes: [] } }") && !w("{ explain: { find: 'x' } }"));
    }

    #[test]
    fn dates_and_decimals() {
        assert_eq!(parse_date("2024-01-02T03:04:05.123Z").unwrap().timestamp_millis(), 1_704_164_645_123);
        assert_eq!(parse_date("2024-01-02 03:04:05").unwrap().timestamp_millis(), 1_704_153_600_000);
        assert!(parse_date("2024-02-30").is_none() && parse_date("garbage").is_none());
        assert_eq!(decimal("9.99").unwrap().to_string(), "9.99");
        assert!(decimal("1.2.3").is_none() && decimal("").is_none());
    }

    #[test]
    fn errors_carry_line_and_column() {
        let e = parse_script("db.x.find()\ndb.y.foo()", "shop").unwrap_err();
        assert_eq!(e.to_string(), "Parse error: Unsupported collection method foo() at line 2, column 11");
    }

    #[test]
    fn read_only_refuses_writes_only() {
        let mut p = ConnectionProfile::new("m", crate::profile::DatabaseKind::Mongodb);
        p.read_only = true;
        let call = |s: &str| parse_script(s, "shop").unwrap().remove(0).call;
        assert_eq!(check_mongo(&p, &call("db.x.deleteMany({})"), "shop"), Err(DbError::ReadOnly));
        assert_eq!(check_mongo(&p, &call("db.x.find()"), "shop"), Ok(()));
    }
}
