//! JSON helpers that compare, name, and print values the way the Crystal
//! implementation does, so both write the same messages.

use serde_json::Value;

/// Equality as Crystal's `JSON::Any#==` has it: numbers compare by value
/// whatever their representation (`1 == 1.0`), objects regardless of key
/// order.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => x == y,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, x)| y.get(key).is_some_and(|y| same(x, y)))
        }
        _ => a == b,
    }
}

/// A value's type as Crystal names it, which type errors report
/// ("got Int64").
pub(crate) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "Nil",
        Value::Bool(_) => "Bool",
        Value::Number(n) if n.is_f64() => "Float64",
        Value::Number(_) => "Int64",
        Value::String(_) => "String",
        Value::Array(_) => "Array(JSON::Any)",
        Value::Object(_) => "Hash(String, JSON::Any)",
    }
}

/// A string as itself, anything else as JSON: how messages show a value.
pub(crate) fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => to_json(other),
    }
}

/// Compact JSON, with floats written as Crystal writes them.
pub(crate) fn to_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, &mut out);
    out
}

fn write_json(value: &Value, out: &mut String) {
    match value {
        Value::Number(n) if n.is_f64() => out.push_str(&float_to_s(n.as_f64().unwrap_or(f64::NAN))),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_json(item, out);
            }
            out.push('}');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// A float as Crystal's `Float64#to_s` writes it: the shortest digits that
/// read back as the same float, in fixed notation from 0.0001 up to 1e15
/// (always with a fraction, `100.0`) and in scientific notation outside it
/// (`1.0e+15`, `1.25e-5`).
pub(crate) fn float_to_s(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }

    // Rust's `{:e}` is also the shortest round-trip form: "-1.25e-5".
    let scientific = format!("{f:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // The decimal point falls after this many digits.
    let point = exponent + 1;

    if (-3..=15).contains(&point) {
        let fixed = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if (point as usize) < digits.len() {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        } else {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        };
        format!("{sign}{fixed}")
    } else {
        let rest = if digits.len() > 1 { &digits[1..] } else { "0" };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{sign}{}.{rest}e{exponent_sign}{}",
            &digits[..1],
            exponent.unsigned_abs()
        )
    }
}

/// A bound in a validation message: a whole number without a fraction.
pub(crate) fn format_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        format!("{}", n as i64)
    } else {
        float_to_s(n)
    }
}

/// The ASCII whitespace Crystal skips around a number.
fn ascii_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

/// An integer as Crystal's `String#to_i64?(strict: true)` reads one:
/// decimal digits with an optional sign, surrounding whitespace allowed.
pub(crate) fn parse_int(s: &str) -> Option<i64> {
    s.trim_matches(ascii_space).parse().ok()
}

/// A float as Crystal's `String#to_f64?(strict: true)` reads one, except
/// that infinities and NaN are refused, since JSON can't hold them.
pub(crate) fn parse_float(s: &str) -> Option<f64> {
    let s = s.trim_matches(ascii_space);
    let f: f64 = s.parse().ok()?;
    f.is_finite().then_some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_as_crystal_prints_them() {
        let cases = [
            (0.1, "0.1"),
            (1.5, "1.5"),
            (100.0, "100.0"),
            (1e15, "1.0e+15"),
            (1.5e16, "1.5e+16"),
            (123456789012345.6, "123456789012345.6"),
            (1e-4, "0.0001"),
            (1.25e-5, "1.25e-5"),
            (0.001, "0.001"),
            (-2.5, "-2.5"),
            (5e-324, "5.0e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (3.0e-5, "3.0e-5"),
            (12345678.9, "12345678.9"),
        ];
        for (f, expected) in cases {
            assert_eq!(float_to_s(f), expected, "{f:e}");
        }
    }

    #[test]
    fn numbers_read_as_crystal_reads_them() {
        assert_eq!(parse_int(" 42 "), Some(42));
        assert_eq!(parse_int("+5"), Some(5));
        assert_eq!(parse_int("007"), Some(7));
        assert_eq!(parse_int("\t7\n"), Some(7));
        assert_eq!(parse_int("-0"), Some(0));
        for bad in [
            "0x10",
            "1_000",
            "1e3",
            "",
            " ",
            "42abc",
            "9223372036854775808",
            "+-5",
        ] {
            assert_eq!(parse_int(bad), None, "{bad:?}");
        }
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("5."), Some(5.0));
        assert_eq!(parse_float(" 1E-2 "), Some(0.01));
        for bad in ["inf", "nan", "1e400", "0x1p3", "1_000.5", "", "1.5x"] {
            assert_eq!(parse_float(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn equality_ignores_number_representation_and_key_order() {
        assert!(same(&json!(5), &json!(5.0)));
        assert!(same(&json!([1, {"a": 1}]), &json!([1.0, {"a": 1.0}])));
        assert!(same(&json!({"a": 1, "b": 2}), &json!({"b": 2, "a": 1})));
        assert!(!same(&json!("1"), &json!(1)));
    }
}
