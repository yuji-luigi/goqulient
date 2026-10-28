//! Port of GokuCore's `BSONFormat`: shell-style text for BSON values (`ObjectId('…')`, `ISODate('…')`, `NumberLong(n)`),
//! the Mongo grid (`_id` first, then keys in first-seen order) and the type names inference uses.
use crate::model::ResultColumn;
use bson::spec::BinarySubtype;
use bson::{Bson, Document};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;

/// A Mongo grid never has more columns than this.
pub const MAX_COLUMNS: usize = 500;

pub fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Null | Bson::Undefined => "null",
        Bson::String(_) | Bson::Symbol(_) => "string",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::Boolean(_) => "bool",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Binary(_) => "binData",
        Bson::RegularExpression(_) => "regex",
        Bson::Timestamp(_) => "timestamp",
        // Swift prints the BSON library's type name for the rest.
        Bson::MinKey => "MinKey",
        Bson::MaxKey => "MaxKey",
        Bson::JavaScriptCode(_) => "JavaScriptCode",
        Bson::JavaScriptCodeWithScope(_) => "JavaScriptCodeWithScope",
        Bson::DbPointer(_) => "DBPointer",
    }
}

/// Cell text: missing and null are `None`, strings are raw, everything else a one-line shell literal.
pub fn cell(v: Option<&Bson>) -> Option<String> {
    match v? {
        Bson::Null | Bson::Undefined => None,
        Bson::String(s) => Some(s.clone()),
        other => Some(literal(other, None)),
    }
}

/// Shell literal; `indent: Some(n)` pretty-prints documents and arrays with `n` spaces per level.
pub fn literal(v: &Bson, indent: Option<usize>) -> String {
    let mut out = String::new();
    write_literal(&mut out, v, indent, 0);
    out
}

/// A whole document as a literal (without cloning it into a `Bson`).
pub fn document_literal(d: &Document, indent: Option<usize>) -> String {
    let mut out = String::new();
    container(&mut out, d.iter().map(|(k, v)| (Some(k), v)), d.is_empty(), false, indent, 0);
    out
}

fn write_literal(out: &mut String, v: &Bson, indent: Option<usize>, depth: usize) {
    if depth > 64 {
        out.push('…');
        return;
    }
    match v {
        Bson::String(s) | Bson::Symbol(s) => out.push_str(&quote(s)),
        Bson::Int32(i) => _ = write!(out, "{i}"),
        Bson::Int64(i) => _ = write!(out, "NumberLong({i})"),
        Bson::Double(d) => out.push_str(&double(*d)),
        Bson::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Bson::Null | Bson::Undefined => out.push_str("null"),
        Bson::ObjectId(o) => _ = write!(out, "ObjectId('{}')", o.to_hex()),
        Bson::DateTime(d) => _ = write!(out, "ISODate('{}')", iso_date(d.timestamp_millis())),
        Bson::Decimal128(d) => _ = write!(out, "NumberDecimal('{d}')"),
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 => {
            _ = write!(out, "UUID('{}')", uuid(&b.bytes))
        }
        Bson::Binary(b) => {
            let shown = &b.bytes[..b.bytes.len().min(4096)];
            _ = write!(out, "BinData(0, '{}'", base64(shown));
            if b.bytes.len() > 4096 {
                _ = write!(out, " /* {} bytes */", b.bytes.len());
            }
            out.push(')');
        }
        Bson::RegularExpression(r) => _ = write!(out, "/{}/{}", r.pattern, r.options),
        Bson::Timestamp(t) => _ = write!(out, "Timestamp({}, {})", t.time, t.increment),
        Bson::MinKey => out.push_str("MinKey()"),
        Bson::MaxKey => out.push_str("MaxKey()"),
        Bson::JavaScriptCode(c) => _ = write!(out, "Code({})", quote(c)),
        Bson::JavaScriptCodeWithScope(c) => _ = write!(out, "Code({})", quote(&c.code)),
        Bson::DbPointer(p) => _ = write!(out, "{p:?}"),
        Bson::Array(a) => container(out, a.iter().map(|v| (None, v)), a.is_empty(), true, indent, depth),
        Bson::Document(d) => container(out, d.iter().map(|(k, v)| (Some(k), v)), d.is_empty(), false, indent, depth),
    }
}

fn container<'a>(
    out: &mut String,
    items: impl Iterator<Item = (Option<&'a String>, &'a Bson)>,
    empty: bool,
    array: bool,
    indent: Option<usize>,
    depth: usize,
) {
    let (open, close) = if array { ('[', ']') } else { ('{', '}') };
    out.push(open);
    if empty {
        out.push(close);
        return;
    }
    match indent {
        None if !array => out.push(' '),
        None => {}
        Some(_) => out.push('\n'),
    }
    for (i, (k, v)) in items.enumerate() {
        if i > 0 {
            out.push_str(if indent.is_some() { ",\n" } else { ", " });
        }
        if let Some(n) = indent {
            out.extend(std::iter::repeat_n(' ', n * (depth + 1)));
        }
        if let Some(k) = k {
            out.push_str(&quote_key(k));
            out.push_str(": ");
        }
        write_literal(out, v, indent, depth + 1);
    }
    match indent {
        None if !array => out.push(' '),
        None => {}
        Some(n) => {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', n * depth));
        }
    }
    out.push(close);
}

pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => _ = write!(out, "\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn quote_key(k: &str) -> String {
    let ok = !k.is_empty()
        && k.chars().all(|c| c.is_alphabetic() || c == '_' || c == '$' || c.is_ascii_digit())
        && !k.chars().next().is_some_and(char::is_numeric);
    if ok { k.to_owned() } else { quote(k) }
}

/// Swift's `Double` text: integral values below 1e15 as `%.1f`, otherwise the shortest round-trip form, exponential
/// (`1e+16`, `1e-05`) below 1e-4 and above 2^53.
pub fn double(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if d == d.round() && d.abs() < 1e15 {
        return format!("{d:.1}");
    }
    let a = d.abs();
    if a != 0.0 && (a < 1e-4 || a > 9_007_199_254_740_992.0) {
        let s = format!("{d:e}");
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        return format!("{mantissa}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    format!("{d:?}")
}

/// `yyyy-MM-ddTHH:mm:ss.SSSZ` in UTC, like Swift's `ISO8601DateFormatter` with fractional seconds.
pub fn iso_date(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let ms = millis.rem_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

fn uuid(b: &[u8]) -> String {
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Documents → grid: `_id` first, then keys in order of first appearance (at most `MAX_COLUMNS`); a column's type is
/// every type seen for it, sorted and joined with `|`.
pub fn grid(docs: &[Document]) -> (Vec<ResultColumn>, Vec<Vec<Option<String>>>) {
    let mut names: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut types: HashMap<&str, BTreeSet<&'static str>> = HashMap::new();
    for d in docs {
        for (k, v) in d {
            if !seen.contains(k.as_str()) && names.len() < MAX_COLUMNS {
                seen.insert(k);
                names.push(k);
            }
            types.entry(k).or_default().insert(type_name(v));
        }
    }
    if let Some(i) = names.iter().position(|n| *n == "_id")
        && i != 0
    {
        let id = names.remove(i);
        names.insert(0, id);
    }
    let columns = names
        .iter()
        .map(|n| ResultColumn {
            name: (*n).to_owned(),
            type_name: types.get(n).map(|t| t.iter().copied().collect::<Vec<_>>().join("|")).unwrap_or_default(),
        })
        .collect();
    let rows = docs.iter().map(|d| names.iter().map(|n| cell(d.get(*n))).collect()).collect();
    (columns, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::oid::ObjectId;
    use bson::{Binary, Decimal128, Timestamp, doc};

    #[test]
    fn doubles_print_like_swift() {
        let cases = [
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1e22, "1e+22"),
            (0.001, "0.001"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (123.456, "123.456"),
            (1e15 + 0.5, "1000000000000000.5"),
            (1.5e300, "1.5e+300"),
            (-2.5e-5, "-2.5e-05"),
            (0.1, "0.1"),
            (5e-324, "5e-324"),
            (3.0, "3.0"),
            (-0.0, "-0.0"),
        ];
        for (d, want) in cases {
            assert_eq!(double(d), want, "{d:?}");
        }
        assert_eq!((double(f64::NAN), double(f64::NEG_INFINITY)), ("NaN".into(), "-Infinity".into()));
    }

    #[test]
    fn dates_match_iso8601_formatter() {
        assert_eq!(iso_date(1_709_212_455_123), "2024-02-29T13:14:15.123Z");
        assert_eq!(iso_date(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(iso_date(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_date(-62_135_596_800_000 + 86_400_000), "0001-01-02T00:00:00.000Z");
        assert_eq!(iso_date(253_402_300_800_000), "10000-01-01T00:00:00.000Z");
    }

    #[test]
    fn literals() {
        let oid = ObjectId::parse_str("65a000000000000000000001").unwrap();
        let uid = Binary { subtype: BinarySubtype::Uuid, bytes: (0u8..16).collect() };
        let d = doc! {
            "_id": oid, "s": "a\"b\n\u{1}", "i": 1, "l": 3_000_000_000i64, "f": 1.5, "t": true, "n": null,
            "dec": "9.99".parse::<Decimal128>().unwrap(), "u": uid,
            "bin": Binary { subtype: BinarySubtype::Generic, bytes: b"hello".to_vec() },
            "re": crate::drivers::mongo::parse_document(r#"{"r":{"$regularExpression":{"pattern":"ab+c","options":"i"}}}"#)
                .unwrap().get("r").unwrap().clone(),
            "ts": Timestamp { time: 1_700_000_000, increment: 1 },
            "arr": [1, "x"], "empty": {}, "1st": 1, "a-b": 1, "ünï": 1,
        };
        assert_eq!(
            literal(&Bson::Document(d.clone()), None),
            "{ _id: ObjectId('65a000000000000000000001'), s: \"a\\\"b\\n\\u0001\", i: 1, l: NumberLong(3000000000), \
             f: 1.5, t: true, n: null, dec: NumberDecimal('9.99'), u: UUID('00010203-0405-0607-0809-0a0b0c0d0e0f'), \
             bin: BinData(0, 'aGVsbG8='), re: /ab+c/i, ts: Timestamp(1700000000, 1), arr: [1, \"x\"], empty: {}, \
             \"1st\": 1, \"a-b\": 1, ünï: 1 }"
        );
        assert_eq!(
            literal(&Bson::Document(doc! {"a": 1, "b": {"c": [1, {}]}}), Some(2)),
            "{\n  a: 1,\n  b: {\n    c: [\n      1,\n      {}\n    ]\n  }\n}"
        );
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b""), "");
        assert_eq!(cell(Some(&Bson::String("raw".into()))), Some("raw".into()));
        assert_eq!((cell(Some(&Bson::Null)), cell(None)), (None, None));
    }

    #[test]
    fn grid_puts_id_first_and_unions_types() {
        let docs = [doc! {"a": 1, "_id": 1}, doc! {"_id": 2, "a": "x", "b": null}];
        let (cols, rows) = grid(&docs);
        let cols: Vec<_> = cols.iter().map(|c| (c.name.as_str(), c.type_name.as_str())).collect();
        assert_eq!(cols, [("_id", "int"), ("a", "int|string"), ("b", "null")]);
        assert_eq!(rows[0], [Some("1".into()), Some("1".into()), None]);
        assert_eq!(rows[1], [Some("2".into()), Some("x".into()), None]);
    }
}
