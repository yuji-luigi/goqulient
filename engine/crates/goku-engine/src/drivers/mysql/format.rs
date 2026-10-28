//! Port of `MySQLValueFormatter.swift`: cell text and result type names, so the grid reads like the app. Text-protocol
//! values arrive as bytes; binary-protocol values arrive typed (`Int`, `Float`, `Date`, …). Never panics on odd bytes.
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::{Column, Value};

const BINARY_CHARSET: u16 = 63;
const HEX_LIMIT: usize = 4096;

pub fn format(value: &Value, column: &Column) -> Option<String> {
    let t = column.column_type();
    Some(match value {
        Value::NULL => return None,
        Value::Bytes(b) if t == ColumnType::MYSQL_TYPE_BIT => bit_value(b),
        Value::Bytes(b) if column.character_set() == BINARY_CHARSET && is_string_like(t) => {
            hex(b).replacen("\\x", "0x", 1)
        }
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => swift_float(f64::from(*f), true),
        Value::Double(d) => swift_float(*d, false),
        Value::Date(y, m, d, h, mi, s, us) => {
            let mut out = format!("{y:04}-{m:02}-{d:02}");
            if !matches!(t, ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE) {
                out += &format!(" {h:02}:{mi:02}:{s:02}{}", fraction(*us));
            }
            out
        }
        Value::Time(neg, days, h, m, s, us) => {
            let hours = u64::from(*days) * 24 + u64::from(*h);
            format!("{}{hours:02}:{m:02}:{s:02}{}", if *neg { "-" } else { "" }, fraction(*us))
        }
    })
}

fn is_string_like(t: ColumnType) -> bool {
    use ColumnType::*;
    matches!(
        t,
        MYSQL_TYPE_BLOB
            | MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_STRING
            | MYSQL_TYPE_VAR_STRING
            | MYSQL_TYPE_VARCHAR
            | MYSQL_TYPE_GEOMETRY
    )
}

/// `PGValueFormatter.hex`: `\x` + up to 4 KiB of hex, then the full size.
fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().min(HEX_LIMIT) * 2 + 2);
    s.push_str("\\x");
    for x in &b[..b.len().min(HEX_LIMIT)] {
        s += &format!("{x:02x}");
    }
    if b.len() > HEX_LIMIT {
        s += &format!("… ({} bytes)", b.len());
    }
    s
}

fn bit_value(b: &[u8]) -> String {
    if b.len() <= 8 { b.iter().fold(0u64, |v, x| (v << 8) | u64::from(*x)).to_string() } else { hex(b) }
}

/// `.123` with trailing zeros removed; empty for 0.
fn fraction(us: u32) -> String {
    if us == 0 {
        return String::new();
    }
    format!(".{us:06}").trim_end_matches('0').to_owned()
}

/// Swift's `description` of a `Float` (`float4`) or a `Double` with a trailing `.0` removed (`PGValueFormatter.formatFloat`):
/// shortest round-trip digits, exponential below 1e-4 or above 2^25 (Float) / 2^54 (Double), like SwiftDtoa.
pub fn swift_float(d: f64, float4: bool) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sci = if float4 { format!("{:e}", (d as f32).abs()) } else { format!("{:e}", d.abs()) };
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let e10 = exp.parse::<i32>().unwrap_or(0) + 1; // value = 0.digits × 10^e10
    let n = digits.len() as i32;
    let limit = if float4 { 33_554_432.0 } else { 18_014_398_509_481_984.0 };
    let mut s = String::from(if d.is_sign_negative() { "-" } else { "" });
    if e10 < -3 || d.abs() > limit {
        s.push_str(&digits[..1]);
        if n > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        let x = e10 - 1;
        s += &format!("e{}{:02}", if x < 0 { '-' } else { '+' }, x.abs());
    } else if e10 <= 0 {
        s += &format!("0.{}{digits}", "0".repeat((-e10) as usize));
    } else if e10 >= n {
        s += &format!("{digits}{}.0", "0".repeat((e10 - n) as usize));
    } else {
        s += &format!("{}.{}", &digits[..e10 as usize], &digits[e10 as usize..]);
    }
    if !float4 && s.ends_with(".0") {
        s.truncate(s.len() - 2);
    }
    s
}

pub fn type_name(c: &Column) -> String {
    use ColumnType::*;
    let t = c.column_type();
    let binary = c.character_set() == BINARY_CHARSET;
    let base = match t {
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "blob"
            } else {
                "text"
            }
        }
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => {
            if binary {
                "varbinary"
            } else {
                "varchar"
            }
        }
        MYSQL_TYPE_STRING => {
            if binary {
                "binary"
            } else {
                "char"
            }
        }
        MYSQL_TYPE_NULL => "null",
        other => return format!("type {}", other as u8),
    };
    let integer =
        matches!(t, MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG);
    if integer && c.flags().contains(ColumnFlags::UNSIGNED_FLAG) { format!("{base} unsigned") } else { base.to_owned() }
}

/// Backtick-quoted identifier (`SQLQuote.identifier(_, .mysql)`).
pub fn ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(t: ColumnType, charset: u16, flags: ColumnFlags) -> Column {
        Column::new(t).with_character_set(charset).with_flags(flags)
    }

    #[test]
    fn floats_print_like_swift() {
        let cases = [
            (1.5, false, "1.5"),
            (2.0, false, "2"),
            (3.0, true, "3.0"),
            (0.1, true, "0.1"),
            (0.0001, false, "0.0001"),
            (0.00001, false, "1e-05"),
            (-0.0, false, "-0"),
            (1e15, false, "1000000000000000"),
            (1e17, false, "1e+17"),
            (1.2345678901234567e20, false, "1.2345678901234567e+20"),
            (16_777_216.0, true, "16777216.0"),
            (1e8, true, "1e+08"),
            (123.456, false, "123.456"),
            (f64::NAN, false, "NaN"),
            (f64::NEG_INFINITY, true, "-Infinity"),
        ];
        for (v, f4, want) in cases {
            assert_eq!(swift_float(v, f4), want, "{v} float4={f4}");
        }
    }

    #[test]
    fn cells_match_the_app() {
        let none = ColumnFlags::empty();
        let bit = col(ColumnType::MYSQL_TYPE_BIT, BINARY_CHARSET, none);
        assert_eq!(format(&Value::Bytes(vec![0x03, 0xff]), &bit).as_deref(), Some("1023"));
        let vb = col(ColumnType::MYSQL_TYPE_VAR_STRING, BINARY_CHARSET, none);
        assert_eq!(format(&Value::Bytes(vec![0xde, 0xad]), &vb).as_deref(), Some("0xdead"));
        let big = format(&Value::Bytes(vec![0; HEX_LIMIT + 1]), &vb).unwrap();
        assert!(big.starts_with("0x0000") && big.ends_with("… (4097 bytes)"));
        let json = col(ColumnType::MYSQL_TYPE_JSON, BINARY_CHARSET, none);
        assert_eq!(format(&Value::Bytes(b"{\"a\": 1}".to_vec()), &json).as_deref(), Some("{\"a\": 1}"));
        let txt = col(ColumnType::MYSQL_TYPE_VAR_STRING, 255, none);
        assert_eq!(format(&Value::Bytes(vec![b'a', 0xff]), &txt).as_deref(), Some("a\u{fffd}"));
        assert_eq!(format(&Value::NULL, &txt), None);
        let date = col(ColumnType::MYSQL_TYPE_DATE, BINARY_CHARSET, none);
        let dt = col(ColumnType::MYSQL_TYPE_DATETIME, BINARY_CHARSET, none);
        assert_eq!(format(&Value::Date(2026, 1, 2, 0, 0, 0, 0), &date).as_deref(), Some("2026-01-02"));
        assert_eq!(format(&Value::Date(0, 0, 0, 0, 0, 0, 0), &dt).as_deref(), Some("0000-00-00 00:00:00"));
        assert_eq!(format(&Value::Date(2026, 1, 2, 3, 4, 5, 120_000), &dt).as_deref(), Some("2026-01-02 03:04:05.12"));
        let time = col(ColumnType::MYSQL_TYPE_TIME, BINARY_CHARSET, none);
        assert_eq!(format(&Value::Time(true, 34, 22, 59, 59, 0), &time).as_deref(), Some("-838:59:59"));
        assert_eq!(format(&Value::Time(false, 0, 1, 2, 3, 500_000), &time).as_deref(), Some("01:02:03.5"));
        assert_eq!(format(&Value::UInt(u64::MAX), &time).as_deref(), Some("18446744073709551615"));
    }

    #[test]
    fn type_names_match_the_app() {
        let u = ColumnFlags::UNSIGNED_FLAG;
        assert_eq!(type_name(&col(ColumnType::MYSQL_TYPE_TINY, BINARY_CHARSET, u)), "tinyint unsigned");
        assert_eq!(type_name(&col(ColumnType::MYSQL_TYPE_DOUBLE, BINARY_CHARSET, u)), "double");
        assert_eq!(type_name(&col(ColumnType::MYSQL_TYPE_BLOB, 255, ColumnFlags::empty())), "text");
        assert_eq!(type_name(&col(ColumnType::MYSQL_TYPE_STRING, BINARY_CHARSET, ColumnFlags::empty())), "binary");
        assert_eq!(ident("a`b"), "`a``b`");
    }
}
