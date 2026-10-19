//! ECMAScript `String(value)` / `Number(value)` for wire `JsValue`s, shared by
//! the skills manager and the run registry.
//!
//! Wire arrays may nest arbitrarily deep (the protocol parser and `Drop` are
//! iterative), while V8's `Array.prototype.join` recurses and throws
//! `RangeError: Maximum call stack size exceeded`. Conversion here is
//! iterative and reports [`StackOverflow`] past [`MAX_ARRAY_DEPTH`] instead of
//! exhausting the Rust stack.
//!
//! Measured (A6.8-pr4 review, Node 22): `String(a)` throws from depth 2516,
//! both at top level and one call deeper. Known divergence: Rust's limit is a
//! fixed constant, while V8's shrinks as the caller's stack grows, so the
//! constant sits below the measured value and depths between it and V8's
//! real limit throw here but convert in TS.
use crate::metadata::js_trim;
use pilotdeck_protocol::{JsValue, stringify_json};

/// Deepest array nesting that converts; conservative against V8's 2515.
pub(crate) const MAX_ARRAY_DEPTH: usize = 2000;

/// V8 `RangeError` from a too-deep `Array.prototype.join`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StackOverflow;
impl StackOverflow {
    pub(crate) const MESSAGE: &'static str = "Maximum call stack size exceeded";
}

/// JS Number#toString (radix 10).
pub(crate) fn number_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        (if n > 0.0 { "Infinity" } else { "-Infinity" }).into()
    } else if n == 0.0 {
        "0".into()
    } else {
        // JSON.stringify uses Number::toString for finite values.
        stringify_json(&JsValue::Number(n))
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}

/// ECMAScript `GetSubstitution` for `String.prototype.replace` when the
/// search value is a string: `$$`, `$&`, `` $` `` and `$'` expand; `$n` and
/// `$<name>` stay literal because a string pattern has no capture groups.
/// `matched`, `prefix` and `suffix` are the matched text and the text on each
/// side of it in the source string.
pub(crate) fn expand_replacement(
    replacement: &str,
    matched: &str,
    prefix: &str,
    suffix: &str,
) -> String {
    let bytes = replacement.as_bytes();
    let mut out = String::with_capacity(replacement.len());
    let mut index = 0usize;
    let mut copied = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$' || index + 1 >= bytes.len() {
            index += 1;
            continue;
        }
        let value = match bytes[index + 1] {
            b'$' => "$",
            b'&' => matched,
            b'`' => prefix,
            b'\'' => suffix,
            _ => {
                index += 1;
                continue;
            }
        };
        out.push_str(&replacement[copied..index]);
        out.push_str(value);
        index += 2;
        copied = index;
    }
    out.push_str(&replacement[copied..]);
    out
}

/// `String(value)` as UTF-16 units (lone surrogates kept). Inside arrays,
/// `null`/`undefined` elements join as empty strings.
pub(crate) fn to_string_units(value: &JsValue) -> Result<Vec<u16>, StackOverflow> {
    let mut out = Vec::new();
    // One frame per array being joined: its remaining items and whether a
    // separator is due before the next one.
    let mut frames: Vec<(std::slice::Iter<'_, JsValue>, bool)> = Vec::new();
    let mut next = Some(value);
    loop {
        match next.take() {
            Some(JsValue::Array(items)) => {
                if frames.len() >= MAX_ARRAY_DEPTH {
                    return Err(StackOverflow);
                }
                frames.push((items.iter(), false));
            }
            Some(scalar) => scalar_units(scalar, &mut out),
            None => {}
        }
        let Some((items, separate)) = frames.last_mut() else {
            return Ok(out);
        };
        match items.next() {
            Some(item) => {
                if *separate {
                    out.push(u16::from(b','));
                }
                *separate = true;
                if !matches!(item, JsValue::Undefined | JsValue::Null) {
                    next = Some(item);
                }
            }
            None => {
                frames.pop();
            }
        }
    }
}
fn scalar_units(value: &JsValue, out: &mut Vec<u16>) {
    match value {
        JsValue::String(s) => out.extend_from_slice(s.units()),
        JsValue::Undefined => out.extend("undefined".encode_utf16()),
        JsValue::Null => out.extend("null".encode_utf16()),
        JsValue::Bool(b) => out.extend(b.to_string().encode_utf16()),
        JsValue::Number(n) => out.extend(number_string(*n).encode_utf16()),
        JsValue::Object(_) => out.extend("[object Object]".encode_utf16()),
        JsValue::Array(_) => unreachable!("arrays are joined by to_string_units"),
    }
}

/// `String(value)` with lone surrogates as U+FFFD (for Rust-side messages).
pub(crate) fn display(value: &JsValue) -> Result<String, StackOverflow> {
    Ok(String::from_utf16_lossy(&to_string_units(value)?))
}

/// `JSON.stringify` with the same conservative container-depth bound as
/// [`to_string_units`]. The serializer itself walks iteratively; this guard
/// makes a too-deep input report V8's `RangeError` instead of silently
/// producing a string. V8's own `JSON.stringify` limit is caller-stack
/// dependent (Node 22 probe: 6500 nesting converts, 7000 throws at top level),
/// so the shared 2000 bound is deliberately conservative rather than an exact
/// threshold.
pub(crate) fn json_stringify(value: &JsValue) -> Result<Option<String>, StackOverflow> {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_ARRAY_DEPTH {
            return Err(StackOverflow);
        }
        match value {
            JsValue::Array(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            JsValue::Object(object) => {
                pending.extend(object.entries().iter().map(|(_, item)| (item, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(stringify_json(value).ok().flatten())
}

/// `Number(value)`.
pub(crate) fn to_number(value: &JsValue) -> Result<f64, StackOverflow> {
    Ok(match value {
        JsValue::Undefined | JsValue::Object(_) => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(b) => f64::from(u8::from(*b)),
        JsValue::Number(n) => *n,
        JsValue::String(s) => string_to_number(&String::from_utf16_lossy(s.units())),
        JsValue::Array(_) => string_to_number(&display(value)?),
    })
}

/// ECMAScript StringToNumber.
pub(crate) fn string_to_number(text: &str) -> f64 {
    let t = js_trim(text);
    if t.is_empty() {
        return 0.0;
    }
    let radix = match t.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 0,
    };
    if radix != 0 {
        let digits = &t[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        // Exact up to 128 bits, then rounded once, like V8.
        return match u128::from_str_radix(digits, radix) {
            Ok(v) => v as f64,
            Err(_) => digits.chars().fold(0.0, |acc, c| {
                acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap())
            }),
        };
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = digits(int) && digits(frac) && !(int.is_empty() && frac.is_empty());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    if mantissa_ok && exponent_ok {
        t.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    fn nested(depth: usize) -> JsValue {
        let mut value = JsValue::Number(7.0);
        for _ in 0..depth {
            value = JsValue::Array(vec![value]);
        }
        value
    }

    #[test]
    fn conversions_follow_ecmascript() {
        for (text, expected) in [
            ("", 0.0),
            ("  12  ", 12.0),
            ("0x10", 16.0),
            ("0b101", 5.0),
            ("0o17", 15.0),
            ("1e3", 1000.0),
            (".5", 0.5),
            ("5.", 5.0),
            ("-Infinity", f64::NEG_INFINITY),
            ("+1", 1.0),
        ] {
            assert_eq!(string_to_number(text), expected, "{text:?}");
        }
        for text in [
            "abc",
            "-0x10",
            "inf",
            "nan",
            "1_000",
            "e5",
            ".",
            "0x",
            "1e",
            "Infinityx",
        ] {
            assert!(string_to_number(text).is_nan(), "{text:?}");
        }
        let json = |t: &str| parse_json(t).unwrap();
        assert_eq!(to_number(&json("[5]")), Ok(5.0));
        assert!(to_number(&json("[1,2]")).unwrap().is_nan());
        assert_eq!(to_number(&json("[]")), Ok(0.0));
        assert_eq!(to_number(&JsValue::Bool(true)), Ok(1.0));
        assert!(to_number(&json("{}")).unwrap().is_nan());
        assert_eq!(number_string(-0.0), "0");
        assert_eq!(number_string(1e21), "1e+21");
        assert_eq!(
            display(&json(r#"[1,null,"a",[2,3],[],{}]"#)).unwrap(),
            "1,,a,2,3,,[object Object]"
        );
        assert_eq!(display(&JsValue::Undefined).unwrap(), "undefined");
        assert_eq!(display(&json("[[],[[]]]")).unwrap(), ",");
        // Lone surrogates survive the unit form.
        let lone = pilotdeck_protocol::JsString::from_utf16(vec![0xd800]);
        let lone = JsValue::Array(vec![JsValue::String(lone)]);
        assert_eq!(to_string_units(&lone).unwrap(), vec![0xd800]);
    }

    #[test]
HEAD
    fn json_stringify_reports_the_shared_depth_bound() {
        let nested_value = nested(MAX_ARRAY_DEPTH);
        assert!(json_stringify(&nested_value).is_ok());
        assert_eq!(
            json_stringify(&nested(MAX_ARRAY_DEPTH + 1)),
            Err(StackOverflow)
        );
        let mut object = JsValue::Object(Default::default());
        for _ in 0..=MAX_ARRAY_DEPTH {
            let mut parent = pilotdeck_protocol::JsObject::new();
            parent.insert("x".into(), object);
            object = JsValue::Object(parent);
        }
        assert_eq!(json_stringify(&object), Err(StackOverflow));

    fn replacement_patterns_follow_get_substitution() {
        assert_eq!(expand_replacement("$$", "m", "pre", "post"), "$");
        assert_eq!(expand_replacement("$&", "m", "pre", "post"), "m");
        assert_eq!(expand_replacement("$`", "m", "pre", "post"), "pre");
        assert_eq!(expand_replacement("$'", "m", "pre", "post"), "post");
        assert_eq!(
            expand_replacement("$1 $<n> $ $x $$ $& $` $'", "m", "pre", "post"),
            "$1 $<n> $ $x $ m pre post"
        );
        assert_eq!(
            expand_replacement("没有 $ 模式", "m", "pre", "post"),
            "没有 $ 模式"
        );
    }

    #[test]
    fn replacement_patterns_follow_get_substitution() {
        assert_eq!(expand_replacement("$$", "m", "pre", "post"), "$");
        assert_eq!(expand_replacement("$&", "m", "pre", "post"), "m");
        assert_eq!(expand_replacement("$`", "m", "pre", "post"), "pre");
        assert_eq!(expand_replacement("$'", "m", "pre", "post"), "post");
        assert_eq!(
            expand_replacement("$1 $<n> $ $x $$ $& $` $'", "m", "pre", "post"),
            "$1 $<n> $ $x $ m pre post"
        );
        assert_eq!(
            expand_replacement("没有 $ 模式", "m", "pre", "post"),
            "没有 $ 模式"
        );
    }

    #[test]
    fn json_stringify_reports_the_shared_depth_bound() {
        let nested_value = nested(MAX_ARRAY_DEPTH);
        assert!(json_stringify(&nested_value).is_ok());
        assert_eq!(
            json_stringify(&nested(MAX_ARRAY_DEPTH + 1)),
            Err(StackOverflow)
        );
        let mut object = JsValue::Object(Default::default());
        for _ in 0..=MAX_ARRAY_DEPTH {
            let mut parent = pilotdeck_protocol::JsObject::new();
            parent.insert("x".into(), object);
            object = JsValue::Object(parent);
        }
        assert_eq!(json_stringify(&object), Err(StackOverflow));
    }

    #[test]
    fn deep_arrays_report_stack_overflow_without_recursing() {
        assert_eq!(display(&nested(MAX_ARRAY_DEPTH)).unwrap(), "7");
        assert_eq!(to_number(&nested(MAX_ARRAY_DEPTH)), Ok(7.0));
        assert_eq!(display(&nested(MAX_ARRAY_DEPTH + 1)), Err(StackOverflow));
        assert_eq!(to_number(&nested(MAX_ARRAY_DEPTH + 1)), Err(StackOverflow));
        // Far past the limit: still an error, never a Rust stack overflow.
        assert_eq!(to_string_units(&nested(1_000_000)), Err(StackOverflow));
    }
}