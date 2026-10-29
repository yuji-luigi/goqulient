//! Port of GokuCore's `PGValueFormatter`: binary-format values (what tokio-postgres always requests) to the text the app's
//! grid shows. Never panics on malformed input: unknown types or short buffers fall back to UTF-8 text or a hex dump.
use std::collections::HashMap;
use std::sync::LazyLock;

/// 2000-01-01 in Unix seconds.
const PG_EPOCH: i64 = 946_684_800;
/// Long binary values are shortened for display.
const HEX_LIMIT: usize = 4096;

struct Buf<'a>(&'a [u8]);

impl<'a> Buf<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.0.len() {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N).and_then(|b| b.try_into().ok())
    }
    fn u8(&mut self) -> Option<u8> {
        self.array::<1>().map(|b| b[0])
    }
    fn i16(&mut self) -> Option<i16> {
        self.array().map(i16::from_be_bytes)
    }
    fn u16(&mut self) -> Option<u16> {
        self.array().map(u16::from_be_bytes)
    }
    fn i32(&mut self) -> Option<i32> {
        self.array().map(i32::from_be_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_be_bytes)
    }
    fn i64(&mut self) -> Option<i64> {
        self.array().map(i64::from_be_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.array().map(u64::from_be_bytes)
    }
    fn rest(&mut self) -> &'a [u8] {
        self.take(self.0.len()).unwrap_or_default()
    }
    /// A length-prefixed slice; Swift's `readSlice(length:)` fails on a negative length.
    fn slice(&mut self, len: i32) -> Option<&'a [u8]> {
        self.take(usize::try_from(len).ok()?)
    }
}

/// Display text for one non-NULL value of type `oid`.
pub fn format(raw: &[u8], oid: u32) -> String {
    decode(&mut Buf(raw), oid).unwrap_or_else(|| fallback(raw))
}

fn fallback(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    if s.chars().any(|c| (c as u32) < 0x20 && !matches!(c, '\n' | '\t' | '\r')) { hex(raw) } else { s.into_owned() }
}

fn hex(raw: &[u8]) -> String {
    let shown = &raw[..raw.len().min(HEX_LIMIT)];
    let mut s = String::with_capacity(shown.len() * 2 + 24);
    s.push_str("\\x");
    for b in shown {
        s.push_str(&format!("{b:02x}"));
    }
    if raw.len() > HEX_LIMIT {
        s.push_str(&format!("… ({} bytes)", raw.len()));
    }
    s
}

fn text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

fn decode(b: &mut Buf, oid: u32) -> Option<String> {
    Some(match oid {
        16 => (if b.u8()? != 0 { "true" } else { "false" }).to_owned(),
        17 => hex(b.rest()),
        18 => text(b.rest()),
        20 => b.i64()?.to_string(),
        21 => b.i16()?.to_string(),
        23 => b.i32()?.to_string(),
        26 | 28 | 29 | 24 | 2202 | 2203 | 2204 | 2205 | 2206 | 3734 | 3769 | 4089 | 4096 => b.u32()?.to_string(),
        700 => format_float(f32::from_bits(b.u32()?) as f64, true),
        701 => format_float(f64::from_bits(b.u64()?), false),
        790 => {
            // money: assumes 2 fractional digits, like en_US lc_monetary
            let cents = b.i64()?;
            let a = cents.unsigned_abs();
            format!("{}${}.{:02}", if cents < 0 { "-" } else { "" }, a / 100, a % 100)
        }
        1700 => numeric(b)?,
        3614 => return None, // tsvector binary is not text
        114 | 142 | 25 | 1042 | 1043 | 19 | 705 | 4072 => {
            if oid == 4072 {
                b.u8(); // jsonpath version byte
            }
            text(b.rest())
        }
        3802 => {
            if b.u8()? != 1 {
                return None;
            }
            text(b.rest())
        }
        2950 => {
            let u = b.take(16)?;
            let h: String = u.iter().map(|x| format!("{x:02x}")).collect();
            format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
        }
        1082 => match b.i32()? {
            i32::MAX => "infinity".into(),
            i32::MIN => "-infinity".into(),
            days => date_string(PG_EPOCH + days as i64 * 86_400, None, true),
        },
        1083 => time_of_day(b.i64()?),
        1266 => {
            let us = b.i64()?;
            let zone = b.i32()?;
            time_of_day(us) + &offset_string(-(zone as i64))
        }
        1114 | 1184 => match b.i64()? {
            i64::MAX => "infinity".into(),
            i64::MIN => "-infinity".into(),
            us => {
                let (secs, frac) = (us.div_euclid(1_000_000), us.rem_euclid(1_000_000));
                date_string(PG_EPOCH + secs, Some(frac), false) + if oid == 1184 { "+00" } else { "" }
            }
        },
        1186 => interval(b)?,
        869 | 650 => inet(b)?,
        829 | 774 => b.rest().iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":"),
        1560 | 1562 => {
            let len = usize::try_from(b.i32()?).ok()?;
            let bytes = b.rest();
            (0..len)
                .filter(|i| i / 8 < bytes.len())
                .map(|i| if (bytes[i / 8] >> (7 - (i % 8))) & 1 == 1 { '1' } else { '0' })
                .collect()
        }
        600 => {
            let (x, y) = (f64::from_bits(b.u64()?), f64::from_bits(b.u64()?));
            format!("({},{})", format_float(x, false), format_float(y, false))
        }
        2249 => record(b)?,
        3904 | 3926 | 3906 | 3908 | 3910 | 3912 => range(b, oid)?,
        _ => array(b, *ARRAY_ELEMENT.get(&oid)?)?,
    })
}

/// Well-known array type OID → element OID.
static ARRAY_ELEMENT: LazyLock<HashMap<u32, u32>> = LazyLock::new(|| {
    HashMap::from([
        (1000, 16),
        (1001, 17),
        (1002, 18),
        (1003, 19),
        (1016, 20),
        (1005, 21),
        (1007, 23),
        (1009, 25),
        (1028, 26),
        (199, 114),
        (143, 142),
        (1021, 700),
        (1022, 701),
        (791, 790),
        (1231, 1700),
        (1014, 1042),
        (1015, 1043),
        (2951, 2950),
        (1182, 1082),
        (1183, 1083),
        (1270, 1266),
        (1115, 1114),
        (1185, 1184),
        (1187, 1186),
        (1041, 869),
        (651, 650),
        (1040, 829),
        (3807, 3802),
        (1561, 1560),
        (1563, 1562),
        (1017, 600),
        (3905, 3904),
        (3927, 3926),
        (3907, 3906),
        (3909, 3908),
        (3911, 3910),
        (3913, 3912),
        (2287, 2249),
    ])
});

/// Swift's `"\(d)"` for float8 without a trailing ".0", `"\(Float(d))"` for float4.
pub(crate) fn format_float(d: f64, float4: bool) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return (if d > 0.0 { "Infinity" } else { "-Infinity" }).into();
    }
    if float4 {
        return swift_description(d as f32);
    }
    let s = swift_description(d);
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

trait SwiftFloat: std::fmt::Display + std::fmt::LowerExp + Copy {
    /// Above this magnitude Swift switches to exponential notation (2^53 for Double, 2^24 for Float).
    const EXACT_INT_LIMIT: f64;
    fn magnitude(self) -> f64;
}
impl SwiftFloat for f64 {
    const EXACT_INT_LIMIT: f64 = 9_007_199_254_740_992.0;
    fn magnitude(self) -> f64 {
        self.abs()
    }
}
impl SwiftFloat for f32 {
    const EXACT_INT_LIMIT: f64 = 16_777_216.0;
    fn magnitude(self) -> f64 {
        self.abs() as f64
    }
}

/// Swift's `description` of a finite Float/Double: shortest round-trip digits; exponential ("1e-05", "1.5e+20") below
/// 1e-4 or above the exact-integer limit, otherwise decimal with at least one fraction digit ("100.0").
fn swift_description<F: SwiftFloat>(v: F) -> String {
    let m = v.magnitude();
    if m == 0.0 {
        return if v.to_string().starts_with('-') { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{v:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if exp < -4 || m > F::EXACT_INT_LIMIT {
        return format!("{mantissa}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.unsigned_abs());
    }
    let s = v.to_string();
    if s.contains('.') { s } else { s + ".0" }
}

fn numeric(b: &mut Buf) -> Option<String> {
    let (ndigits, weight, sign, dscale) = (b.i16()?, b.i16()?, b.u16()?, b.i16()?);
    if ndigits < 0 || dscale < 0 {
        return None;
    }
    match sign {
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => {}
    }
    let mut digits = Vec::with_capacity(ndigits as usize);
    for _ in 0..ndigits {
        let d = b.i16()?;
        if !(0..10000).contains(&d) {
            return None;
        }
        digits.push(d);
    }
    let digit = |i: i64| if i >= 0 && (i as usize) < digits.len() { digits[i as usize] } else { 0 };
    let mut int_part = String::new();
    if weight < 0 {
        int_part.push('0');
    } else {
        for i in 0..=weight as i64 {
            if i == 0 {
                int_part += &digit(0).to_string();
            } else {
                int_part += &format!("{:04}", digit(i));
            }
        }
    }
    let mut frac = String::new();
    let mut i = weight as i64 + 1;
    while frac.len() < dscale as usize {
        frac += &format!("{:04}", digit(i));
        i += 1;
    }
    frac.truncate(dscale as usize);
    let neg = if sign == 0x4000 { "-" } else { "" };
    Some(if frac.is_empty() { format!("{neg}{int_part}") } else { format!("{neg}{int_part}.{frac}") })
}

fn time_of_day(micros: i64) -> String {
    let (total, us) = (micros / 1_000_000, micros % 1_000_000);
    let mut s = format!("{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60);
    if us != 0 {
        s += &fraction(us);
    }
    s
}

fn fraction(us: i64) -> String {
    format!(".{us:06}").trim_end_matches('0').to_owned()
}

fn offset_string(seconds: i64) -> String {
    let a = seconds.unsigned_abs();
    let mut s = format!("{}{:02}", if seconds < 0 { '-' } else { '+' }, a / 3600);
    if !a.is_multiple_of(3600) {
        s += &format!(":{:02}", (a / 60) % 60);
    }
    s
}

/// Civil-from-days (proleptic Gregorian), handles BC years like Swift's port.
fn date_string(unix_seconds: i64, micros: Option<i64>, date_only: bool) -> String {
    let (days, sec_of_day) = (unix_seconds.div_euclid(86_400), unix_seconds.rem_euclid(86_400));
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    let bc = y <= 0;
    let mut s = format!("{:04}-{m:02}-{d:02}", if bc { 1 - y } else { y });
    if !date_only {
        s += &format!(" {:02}:{:02}:{:02}", sec_of_day / 3600, (sec_of_day / 60) % 60, sec_of_day % 60);
        if let Some(us) = micros.filter(|&us| us != 0) {
            s += &fraction(us);
        }
    }
    if bc { s + " BC" } else { s }
}

fn interval(b: &mut Buf) -> Option<String> {
    let (us, days, months) = (b.i64()?, b.i32()?, b.i32()?);
    let plural = |n: i32| if n.unsigned_abs() == 1 { "" } else { "s" };
    let mut parts = Vec::new();
    let (years, mons) = (months / 12, months % 12);
    if years != 0 {
        parts.push(format!("{years} year{}", plural(years)));
    }
    if mons != 0 {
        parts.push(format!("{mons} mon{}", plural(mons)));
    }
    if days != 0 {
        parts.push(format!("{days} day{}", plural(days)));
    }
    if us != 0 || parts.is_empty() {
        let a = us.unsigned_abs();
        let (secs, frac) = (a / 1_000_000, a % 1_000_000);
        let mut t = format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60);
        if frac != 0 {
            t += &fraction(frac as i64);
        }
        parts.push(if us < 0 { format!("-{t}") } else { t });
    }
    Some(parts.join(" "))
}

fn inet(b: &mut Buf) -> Option<String> {
    let (family, bits, is_cidr, n) = (b.u8()?, b.u8()?, b.u8()?, b.u8()?);
    let addr = b.take(n as usize)?;
    if family == 2 && addr.len() == 4 {
        let text = addr.iter().map(u8::to_string).collect::<Vec<_>>().join(".");
        return Some(if bits == 32 && is_cidr == 0 { text } else { format!("{text}/{bits}") });
    }
    if addr.len() == 16 {
        let groups: Vec<String> = addr.chunks(2).map(|g| format!("{:x}", (g[0] as u16) << 8 | g[1] as u16)).collect();
        let text = compress_ipv6(&groups);
        return Some(if bits == 128 && is_cidr == 0 { text } else { format!("{text}/{bits}") });
    }
    None
}

fn compress_ipv6(g: &[String]) -> String {
    let (mut best_start, mut best_len, mut i) = (None, 0, 0);
    while i < g.len() {
        if g[i] == "0" {
            let mut j = i;
            while j < g.len() && g[j] == "0" {
                j += 1;
            }
            if j - i > best_len && j - i > 1 {
                (best_start, best_len) = (Some(i), j - i);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    match best_start {
        None => g.join(":"),
        Some(s) => format!("{}::{}", g[..s].join(":"), g[s + best_len..].join(":")),
    }
}

fn array(b: &mut Buf, declared: u32) -> Option<String> {
    let ndim = b.i32()?;
    if !(0..=6).contains(&ndim) {
        return None;
    }
    b.i32()?; // flags
    let elem_oid = b.u32()?;
    if ndim == 0 {
        return Some("{}".into());
    }
    let mut dims = Vec::with_capacity(ndim as usize);
    for _ in 0..ndim {
        let size = usize::try_from(b.i32()?).ok()?;
        b.i32()?; // lower bound
        dims.push(size);
    }
    let total = dims.iter().try_fold(1usize, |acc, &d| acc.checked_mul(d)).filter(|&t| t <= 50_000_000)?;
    let mut elements = Vec::with_capacity(total.min(4096));
    for _ in 0..total {
        let len = b.i32()?;
        if len < 0 {
            elements.push("NULL".to_owned());
            continue;
        }
        let slice = b.slice(len)?;
        elements.push(quote_array_element(&format(slice, if elem_oid != 0 { elem_oid } else { declared })));
    }
    fn build(level: usize, dims: &[usize], elements: &[String], idx: &mut usize) -> String {
        if level == dims.len() - 1 {
            let part = elements[*idx..*idx + dims[level]].join(",");
            *idx += dims[level];
            return format!("{{{part}}}");
        }
        let inner: Vec<String> = (0..dims[level]).map(|_| build(level + 1, dims, elements, idx)).collect();
        format!("{{{}}}", inner.join(","))
    }
    Some(build(0, &dims, &elements, &mut 0))
}

fn quote_array_element(s: &str) -> String {
    if s.is_empty() || s.to_uppercase() == "NULL" || s.chars().any(|c| "{},\"\\ ".contains(c) || c.is_whitespace()) {
        return format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    }
    s.to_owned()
}

fn record(b: &mut Buf) -> Option<String> {
    let n = b.i32()?;
    if !(0..=1600).contains(&n) {
        return None;
    }
    let mut fields = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (oid, len) = (b.u32()?, b.i32()?);
        if len < 0 {
            fields.push(String::new());
            continue;
        }
        let t = format(b.slice(len)?, oid);
        fields.push(if t.chars().any(|c| ",()\" ".contains(c)) {
            format!("\"{}\"", t.replace('"', "\"\""))
        } else {
            t
        });
    }
    Some(format!("({})", fields.join(",")))
}

fn range(b: &mut Buf, range_oid: u32) -> Option<String> {
    let flags = b.u8()?;
    let sub = match range_oid {
        3904 => 23,
        3926 => 20,
        3906 => 1700,
        3908 => 1114,
        3910 => 1184,
        3912 => 1082,
        _ => return None,
    };
    if flags & 0x01 != 0 {
        return Some("empty".into());
    }
    let mut bound = |present: bool| -> Option<String> {
        if !present {
            return Some(String::new());
        }
        let len = b.i32()?;
        Some(format(b.slice(len.max(0))?, sub))
    };
    let lower = bound(flags & 0x08 == 0)?;
    let upper = bound(flags & 0x10 == 0)?;
    Some(format!(
        "{}{lower},{upper}{}",
        if flags & 0x02 != 0 { '[' } else { '(' },
        if flags & 0x04 != 0 { ']' } else { ')' }
    ))
}

/// Readable names for common type OIDs (others are resolved through `format_type`), as in Swift.
pub static TYPE_NAMES: LazyLock<HashMap<u32, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        (16, "bool"),
        (17, "bytea"),
        (18, "char"),
        (19, "name"),
        (20, "int8"),
        (21, "int2"),
        (23, "int4"),
        (25, "text"),
        (26, "oid"),
        (114, "json"),
        (142, "xml"),
        (600, "point"),
        (650, "cidr"),
        (700, "float4"),
        (701, "float8"),
        (705, "unknown"),
        (774, "macaddr8"),
        (790, "money"),
        (829, "macaddr"),
        (869, "inet"),
        (1042, "bpchar"),
        (1043, "varchar"),
        (1082, "date"),
        (1083, "time"),
        (1114, "timestamp"),
        (1184, "timestamptz"),
        (1186, "interval"),
        (1266, "timetz"),
        (1560, "bit"),
        (1562, "varbit"),
        (1700, "numeric"),
        (2249, "record"),
        (2950, "uuid"),
        (3802, "jsonb"),
        (3904, "int4range"),
        (3926, "int8range"),
        (3906, "numrange"),
        (3908, "tsrange"),
        (3910, "tstzrange"),
        (3912, "daterange"),
        (1000, "bool[]"),
        (1005, "int2[]"),
        (1007, "int4[]"),
        (1016, "int8[]"),
        (1009, "text[]"),
        (1015, "varchar[]"),
        (1231, "numeric[]"),
        (2951, "uuid[]"),
        (3807, "jsonb[]"),
        (1021, "float4[]"),
        (1022, "float8[]"),
    ])
});

#[cfg(test)]
mod tests {
    use super::*;

    fn be(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn floats_print_like_swift() {
        // Expected strings are Swift's own `print` output (Double/Float description).
        let doubles = [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (0.0001, "0.0001"),
            (0.00012, "0.00012"),
            (0.00001, "1e-05"),
            (1e15, "1000000000000000"),
            (1e16, "1e+16"),
            (9_007_199_254_740_992.0, "9007199254740992"),
            (9_007_199_254_740_994.0, "9.007199254740994e+15"),
            (123_456_789_012_345_680.0, "1.2345678901234568e+17"),
            (1.25e-7, "1.25e-07"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (100000.0, "100000"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (d, want) in doubles {
            assert_eq!(format_float(d, false), want, "{d}");
        }
        let floats: [(f32, &str); 10] = [
            (0.0, "0.0"),
            (1.0, "1.0"),
            (0.1, "0.1"),
            (0.00001, "1e-05"),
            (16_777_216.0, "16777216.0"),
            (33_554_432.0, "3.3554432e+07"),
            (1e8, "1e+08"),
            (3.4e38, "3.4e+38"),
            (123456.7, "123456.7"),
            (0.000123, "0.000123"),
        ];
        for (f, want) in floats {
            assert_eq!(format_float(f as f64, true), want, "{f}");
        }
    }

    #[test]
    fn scalars() {
        assert_eq!(format(&[1], 16), "true");
        assert_eq!(format(&(-7i32).to_be_bytes(), 23), "-7");
        assert_eq!(format(&(-1234i64).to_be_bytes(), 790), "-$12.34");
        assert_eq!(format(&1.0f32.to_bits().to_be_bytes(), 700), "1.0");
        assert_eq!(format(&[1, 2, 0xff], 17), "\\x0102ff");
        assert_eq!(format(&[1, b'{', b'}'], 3802), "{}");
        let uuid: Vec<u8> = (0..16).collect();
        assert_eq!(format(&uuid, 2950), "00010203-0405-0607-0809-0a0b0c0d0e0f");
        // numeric 12.340: ndigits 2, weight 0, sign 0, dscale 3, digits [12, 3400]
        let n = be(&[
            &2i16.to_be_bytes(),
            &0i16.to_be_bytes(),
            &0u16.to_be_bytes(),
            &3i16.to_be_bytes(),
            &12i16.to_be_bytes(),
            &3400i16.to_be_bytes(),
        ]);
        assert_eq!(format(&n, 1700), "12.340");
        // -0.001: ndigits 1, weight -1, sign 0x4000, dscale 3, digits [10]
        let n = be(&[
            &1i16.to_be_bytes(),
            &(-1i16).to_be_bytes(),
            &0x4000u16.to_be_bytes(),
            &3i16.to_be_bytes(),
            &10i16.to_be_bytes(),
        ]);
        assert_eq!(format(&n, 1700), "-0.001");
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(format(&0i32.to_be_bytes(), 1082), "2000-01-01");
        assert_eq!(format(&(-730_120i32).to_be_bytes(), 1082), "0001-12-31 BC");
        assert_eq!(format(&(-730_485i32).to_be_bytes(), 1082), "0001-01-01 BC");
        assert_eq!(format(&i32::MAX.to_be_bytes(), 1082), "infinity");
        assert_eq!(format(&(-1i64).to_be_bytes(), 1114), "1999-12-31 23:59:59.999999");
        assert_eq!(format(&86_400_000_000i64.to_be_bytes(), 1184), "2000-01-02 00:00:00+00");
        assert_eq!(format(&45_296_500_000i64.to_be_bytes(), 1083), "12:34:56.5");
        let tz = be(&[&43_200_000_000i64.to_be_bytes(), &(-19_800i32).to_be_bytes()]);
        assert_eq!(format(&tz, 1266), "12:00:00+05:30");
        let iv = be(&[&(-3_723_000_001i64).to_be_bytes(), &1i32.to_be_bytes(), &14i32.to_be_bytes()]);
        assert_eq!(format(&iv, 1186), "1 year 2 mons 1 day -01:02:03.000001");
        assert_eq!(format(&be(&[&0i64.to_be_bytes(), &0i32.to_be_bytes(), &0i32.to_be_bytes()]), 1186), "00:00:00");
    }

    #[test]
    fn network_bits_and_containers() {
        assert_eq!(format(&[2, 32, 0, 4, 192, 168, 0, 1], 869), "192.168.0.1");
        assert_eq!(format(&[2, 8, 1, 4, 10, 0, 0, 0], 650), "10.0.0.0/8");
        let mut v6 = vec![3, 128, 0, 16];
        v6.extend([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(format(&v6, 869), "2001:db8::1");
        assert_eq!(format(&[8, 0, 0x2b, 1, 2, 3], 829), "08:00:2b:01:02:03");
        assert_eq!(format(&be(&[&3i32.to_be_bytes(), &[0b1010_0000]]), 1560), "101");
        // int4[] {1,NULL,3}
        let arr = be(&[
            &1i32.to_be_bytes(),
            &1i32.to_be_bytes(),
            &23u32.to_be_bytes(),
            &3i32.to_be_bytes(),
            &1i32.to_be_bytes(),
            &4i32.to_be_bytes(),
            &1i32.to_be_bytes(),
            &(-1i32).to_be_bytes(),
            &4i32.to_be_bytes(),
            &3i32.to_be_bytes(),
        ]);
        assert_eq!(format(&arr, 1007), "{1,NULL,3}");
        assert_eq!(quote_array_element("a b"), "\"a b\"");
        assert_eq!(quote_array_element("null"), "\"null\"");
        assert_eq!(quote_array_element("x\"\\"), "\"x\\\"\\\\\"");
        // int4range [1,5)
        let r = be(&[&[0x02], &4i32.to_be_bytes(), &1i32.to_be_bytes(), &4i32.to_be_bytes(), &5i32.to_be_bytes()]);
        assert_eq!(format(&r, 3904), "[1,5)");
        assert_eq!(format(&[0x01], 3904), "empty");
        let rec = be(&[
            &2i32.to_be_bytes(),
            &23u32.to_be_bytes(),
            &4i32.to_be_bytes(),
            &1i32.to_be_bytes(),
            &25u32.to_be_bytes(),
            &3i32.to_be_bytes(),
            b"a b",
        ]);
        assert_eq!(format(&rec, 2249), "(1,\"a b\")");
    }

    #[test]
    fn malformed_input_never_panics() {
        assert_eq!(format(&[1, 2], 23), "\\x0102", "short int falls back to hex (control bytes)");
        assert_eq!(format(b"hello", 99999), "hello", "unknown type as text");
        assert_eq!(format(&[0, 0, 0, 7], 1007), "\\x00000007", "array with 7 dims is rejected");
        let huge = be(&[
            &6i32.to_be_bytes(),
            &0i32.to_be_bytes(),
            &23u32.to_be_bytes(),
            &[0x7f, 0xff, 0xff, 0xff, 0, 0, 0, 1].repeat(6),
        ]);
        assert!(format(&huge, 1007).starts_with("\\x"), "overflowing dimensions are rejected");
        for oid in [16, 20, 700, 701, 1700, 1082, 1083, 1114, 1186, 869, 1560, 600, 2249, 3904, 1007, 3802, 2950] {
            for len in 0..40 {
                let junk: Vec<u8> = (0..len).map(|i| (i * 37 + oid as usize) as u8 | 0x80).collect();
                let _ = format(&junk, oid);
            }
        }
        let long = vec![0u8; HEX_LIMIT + 1];
        assert!(format(&long, 17).ends_with(&format!("… ({} bytes)", HEX_LIMIT + 1)));
    }
}
