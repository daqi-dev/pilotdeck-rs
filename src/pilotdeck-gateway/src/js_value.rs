//! ECMAScript `String(value)` / `Number(value)` for wire `JsValue`s, shared by
//! the skills manager and the run registry. [`ascii_fold_haystack`] keeps
//! ported `(?i)` regexes on the non-unicode `/i` fold.
//!
//! Wire arrays may nest arbitrarily deep (the protocol parser and `Drop` are
//! iterative), while V8's `Array.prototype.join` recurses and throws
//! `RangeError: Maximum call stack size exceeded`. Conversion here is
//! iterative and reports [`StackOverflow`] past [`MAX_ARRAY_DEPTH`] instead of
//! exhausting the Rust stack.
//!
//! Measured (A6.8-pr4 review, Node 22): `String(a)` throws from depth 2516,
//! both at top level and one call deeper; the A6-JSVALUE-DEPTH-001 oracle
//! (#1550) has 2100 converting and 3000 throwing, and nested objects trip it
//! as early as 1887. Known divergence: Rust's limit is a fixed constant, while
//! V8's varies with the caller's stack shape, so the constant sits low and
//! depths between it and V8's real limit throw here but convert in TS.
//! Ledger: specs/gateway-parity/accepted-diffs.md A6-JSVALUE-DEPTH-001 (unapproved).
use pilotdeck_protocol::{JsString, JsValue};

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
    pilotdeck_protocol::number::number_to_string(n)
}

/// Masks the two non-ASCII code points whose Unicode *simple* case folding
/// maps onto ASCII letters, so a Unicode `(?i)` regex over the masked text
/// behaves like a JS `/i` regex without the `u` flag.
///
/// U+017F `ſ` folds to `s` and U+212A `K` (Kelvin sign) folds to `k` in the
/// `regex` crate's Unicode simple folding, but V8's non-unicode `i` refuses to
/// fold any non-ASCII code unit onto an ASCII one. Replacing both with U+FFFD
/// removes those folds; the replacement is inside every wildcard class the
/// ported patterns use, so it cannot change whether a wildcard matches.
pub(crate) fn ascii_fold_haystack(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(['\u{017F}', '\u{212A}']) {
        std::borrow::Cow::Owned(text.replace(['\u{017F}', '\u{212A}'], "\u{FFFD}"))
    } else {
        std::borrow::Cow::Borrowed(text)
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

/// Why a full `String(value)` (with `ToPrimitive`) throws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToStringError {
    /// `RangeError: Maximum call stack size exceeded` (too-deep array join).
    StackOverflow,
    /// `TypeError: Cannot convert object to primitive value`: a wire object
    /// with a `toString` property (JSON cannot hold a callable, so it
    /// shadows `Object.prototype.toString`, and `valueOf` returns the object).
    NotPrimitive,
}
impl ToStringError {
    pub(crate) const NOT_PRIMITIVE: &'static str = "Cannot convert object to primitive value";
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::StackOverflow => "RangeError",
            Self::NotPrimitive => "TypeError",
        }
    }
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::StackOverflow => StackOverflow::MESSAGE,
            Self::NotPrimitive => Self::NOT_PRIMITIVE,
        }
    }
}

/// Both throw surfaces of `String(value)`: an own `toString` property (JSON
/// cannot carry a callable one, so `ToPrimitive` throws) and nesting past
/// [`MAX_ARRAY_DEPTH`]. An explicit stack keeps the walk off the Rust call
/// stack, like every other container traversal here; errors are reported in
/// element order, like the joining walk.
fn validate_coercion(value: &JsValue) -> Result<(), ToStringError> {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            JsValue::Array(items) => {
                if depth >= MAX_ARRAY_DEPTH {
                    return Err(ToStringError::StackOverflow);
                }
                pending.extend(items.iter().rev().map(|item| (item, depth + 1)));
            }
            JsValue::Object(object) if object.get_property("toString").is_some() => {
                return Err(ToStringError::NotPrimitive);
            }
            _ => {}
        }
    }
    Ok(())
}

/// `String(value)` including `ToPrimitive` on objects, also inside arrays
/// (node probe, A6.4-pr7 修复轮 4: `{toString:null}` and `[{toString:null}]`
/// throw `TypeError: Cannot convert object to primitive value`).
pub(crate) fn to_string_units_checked(value: &JsValue) -> Result<Vec<u16>, ToStringError> {
    validate_coercion(value)?;
    to_string_units(value).map_err(|_| ToStringError::StackOverflow)
}

/// Validation-only entry point for callers that only need "would
/// `String(value)` throw?": same walk, no joined text materialized, so a wide
/// array costs no copy of its own stringification.
pub(crate) fn string_coercible_checked(value: &JsValue) -> Result<(), ToStringError> {
    validate_coercion(value)
}

/// `String(value)` as UTF-16 units (lone surrogates kept). Inside arrays,
/// `null`/`undefined` elements join as empty strings. Objects always become
/// `[object Object]`; use [`to_string_units_checked`] where an own
/// `toString` must make the conversion throw.
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
        // `String(new Set())` is `[object Set]`: the tag of a collapsed YAML
        // collection survives into `JsValue`, and dropping it here made an
        // array join print `[object Object]` where TS keeps the class
        // (#1550 修 r6).
        JsValue::Object(o) => {
            out.extend("[object ".encode_utf16());
            out.extend(o.native_tag().unwrap_or("Object").encode_utf16());
            out.push(u16::from(b']'));
        }
        JsValue::Array(_) => unreachable!("arrays are joined by to_string_units"),
    }
}

/// `String(value)` with lone surrogates as U+FFFD (for Rust-side messages).
pub(crate) fn display(value: &JsValue) -> Result<String, StackOverflow> {
    Ok(String::from_utf16_lossy(&to_string_units(value)?))
}

/// A route value as a JS property key. Routes cross module wires as-is
/// (`routerModelInvokerAdapter.ts:30-31`), so a lookup face that indexes its
/// config — `config.providers[providerId]` — coerces exactly like
/// `String(value)`: `undefined` reads as `"undefined"`, numbers as their
/// decimal form, plain objects as `[object Object]`. A value whose `String()`
/// throws (`toString` shadowing an object's, a too-deep array join) reports
/// the JS error instead of substituting a made-up key.
pub(crate) fn property_key(value: &JsValue) -> Result<JsString, ToStringError> {
    to_string_units_checked(value).map(JsString::from_utf16)
}

/// `JSON.stringify` with the same conservative container-depth bound as
/// [`to_string_units`]. The serializer itself walks iteratively; this guard
/// makes a too-deep input report V8's `RangeError` instead of silently
/// producing a string. V8's own `JSON.stringify` limit is caller-stack
/// dependent (Node 22 probe: 6500 nesting converts, 7000 throws at top level),
/// so the shared 2000 bound is deliberately conservative rather than an exact
/// threshold.
pub(crate) fn json_stringify(value: &JsValue) -> Result<Option<String>, StackOverflow> {
    pilotdeck_budget::coercion::json_stringify(value).map_err(|_| StackOverflow)
}
pub(crate) fn to_number(value: &JsValue) -> Result<f64, StackOverflow> {
    pilotdeck_budget::coercion::to_number(value).map_err(|_| StackOverflow)
}
pub(crate) fn string_to_number(text: &str) -> f64 {
    pilotdeck_budget::coercion::string_to_number(text)
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

    /// Untrusted wire values reach this walk from `storage` and `session_model`,
    /// so it must not spend Rust stack: 128 KiB is the small-stack size the
    /// protocol decoder is pinned on.
    #[test]
    fn coercion_validation_survives_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                assert_eq!(
                    string_coercible_checked(&nested(MAX_ARRAY_DEPTH + 1)),
                    Err(ToStringError::StackOverflow)
                );
                assert!(string_coercible_checked(&nested(MAX_ARRAY_DEPTH)).is_ok());
                let wide = JsValue::Array(vec![JsValue::Number(7.0); 100_000]);
                assert!(string_coercible_checked(&wide).is_ok());
            })
            .expect("spawn small-stack thread")
            .join()
            .expect("the coercion walk must fit a 128 KiB stack");
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
