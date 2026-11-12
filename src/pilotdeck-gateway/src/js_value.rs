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
//! Measured (A6.8-pr4 review, Node 22, `String(a)` called at module top
//! level and one call deeper — that stack prefix, not a bare number, is the
//! record): `String(a)` throws from depth 2516; the A6-JSVALUE-DEPTH-001
//! oracle (#1550) has 2100 converting and 3000 throwing, and nested objects
//! trip it as early as 1887. The same V8 recursion bound measured at the run
//! registry's own load call shape folds one level deeper
//! (`storage::run_registry::TS_KEY_FOLD_DEPTH`, #2692 fix-r6 probe on Node
//! v22.22.0 / v8 12.4.254.21-node.33: 2560 folds, 2561 throws) — two stack
//! prefixes, two boundaries, not a contradiction. Known divergence: Rust's
//! limit is a fixed constant, while V8's varies with the caller's stack
//! shape, so the constant sits low and depths between it and V8's real limit
//! throw here but convert in TS.
//! Ledger: specs/gateway-parity/accepted-diffs.md A6-JSVALUE-DEPTH-001 (unapproved).
use crate::metadata::js_trim;
use pilotdeck_protocol::{JsString, JsValue, stringify_json};

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
            JsValue::Object(object) => {
                // ToPrimitive throws when neither `valueOf` nor `toString` is
                // usable. JSON cannot carry a callable, so any *visible*
                // `toString` is a non-callable one that shadows the inherited
                // method (the fix-r1 shape, including through the explicit
                // prototype chain a config merge installs — `get_property`
                // walks it). A chain that terminates at `null` (the
                // `Object.create(null)` / `__proto__: null` shape, #2301
                // fix-r3) finds neither method at all and throws the same
                // TypeError; a missing explicit prototype is the *implicit*
                // `Object.prototype` (both methods callable, no throw), and a
                // non-object prototype value is ignored by JS the same way.
                if object.get_property("toString").is_some() {
                    return Err(ToStringError::NotPrimitive);
                }
                let mut prototype = object.prototype();
                while let Some(JsValue::Object(inner)) = prototype {
                    prototype = inner.prototype();
                }
                if matches!(prototype, Some(JsValue::Null)) {
                    return Err(ToStringError::NotPrimitive);
                }
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

/// The template-literal spelling TS's composite run key gives a session key:
/// `` `${projectKey ?? ""}\0${sessionKey}\0${runId}` `` (RunRegistry.ts:70).
/// The stored record body keeps the raw value while the index and every
/// read-back path key by this spelling, so a record whose key cannot be
/// re-spelled (a too-deep array) is unreadable — the registry read path
/// skips such records instead of failing the whole snapshot (#2692: a
/// fold-to-`""` here used to admit them and brick the registry on reopen,
/// because `to_string_units` reports the same overflow on the way back).
///
/// Inside the fold band (≤ the registry's `TS_KEY_FOLD_DEPTH`, measured at
/// the load call shape) TS's own template spelling folds a deep container to
/// `""` or `"1"`; beyond the band the same template literal throws
/// `Maximum call stack size exceeded` just like `structuredClone` does — the
/// spelling and the clone ride the same V8 recursion bound (the #2692 fix-r6
/// load-point probe folded and threw at identical levels for both leaf
/// shapes). Rust's spelling walks with a depth bound instead, so the overflow
/// surfaces here — with the same message the clone reports.
pub(crate) fn template_key_checked(value: &JsValue) -> Result<JsString, ToStringError> {
    // Same walk as `property_key` — `String()`'s coercion — under a different
    // semantic hat: this is a template-literal slot, that is a property-key
    // slot. TS renders both through the same ToString; only the callers'
    // intent differs, so one spelling function serves both (review P3#4:
    // two verbatim-identical bodies would drift).
    property_key(value)
}

/// The raw-value key TS's router-shaped maps use (SameValueZero): sessions,
/// in-flight turns, the turn-event coordinator, the completion fence, and
/// the busy gates that pre-check `hasActiveTurn` all index by the request's
/// original value, so `null` and the string `"null"` are different keys
/// (#2692 `ts-prodkey-probe.out`, cells b–d). Strings are their own key;
/// every other category is tagged so its spelling cannot collide with a
/// genuine string key: `\0\0raw:null`, `\0\0raw:undefined`,
/// `\0\0raw:true` / `\0\0raw:false`, `\0\0raw:number:<Number string>`
/// (`-0` and `+0` spell alike, matching SameValueZero), and
/// `\0\0raw:object:<spelling>` for containers. A container whose spelling
/// throws answers the error instead of a key — the submit path rejects it
/// up front, the read paths treat it as a miss. A literal string equal to a
/// tag collides with the tagged key; containers key structurally where TS
/// keys by reference — both are registered divergences (R15).
pub(crate) fn router_key(value: &JsValue) -> Result<JsString, ToStringError> {
    const TAG: &str = "\0\0raw:";
    let tagged = |text: String| JsString::from(text.as_str());
    Ok(match value {
        JsValue::String(s) => s.clone(),
        JsValue::Null => tagged(format!("{TAG}null")),
        JsValue::Undefined => tagged(format!("{TAG}undefined")),
        JsValue::Bool(true) => tagged(format!("{TAG}true")),
        JsValue::Bool(false) => tagged(format!("{TAG}false")),
        JsValue::Number(n) => tagged(format!("{TAG}number:{}", number_string(*n))),
        JsValue::Array(_) | JsValue::Object(_) => {
            let units = to_string_units_checked(value)?;
            tagged(format!("{TAG}object:{}", String::from_utf16_lossy(&units)))
        }
    })
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

/// The error `Number(value)` raises when the value's coercion throws —
/// `TypeError: Cannot convert object to primitive value` (toPrimitive finds
/// nothing usable: the inherited `valueOf` returns the object itself, and the
/// only `toString` is one that is not callable) or `RangeError: Maximum call
/// stack size exceeded` (a too-deep array join). Node v22.22.0 probes:
/// `_tmp/20261026-issue-2301/fix-r1/node-probe/probe1.out` (own `toString`
/// shadowing the inherited callable) and `fix-r2/node-probe/probe1.out`
/// (array element, nested array, prototype-carried `toString` via the real
/// `mergeConfigSources`, deep array → RangeError). This is the same walk
/// `String(value)` takes (`validate_coercion`): `Number([x])` joins the
/// array first, so an element's failed toPrimitive and a too-deep nesting
/// throw exactly like the scalar case, and the prototype chain a config
/// merge installed is visible to `get_property`. The loader mounts run this
/// at construction, where the TS constructors' `Number(options.port ?? …)`
/// throws and the loader's catch records the key and continues (#2301 fix-r1;
/// fix-r2 review P1 #3 closed the array / prototype-chain gaps, fix-r3 review
/// P1 #2 the `__proto__: null` / `Object.create(null)` family — a chain that
/// terminates at null finds neither `valueOf` nor `toString` at all).
pub(crate) fn number_conversion_error(value: &JsValue) -> Option<String> {
    validate_coercion(value)
        .err()
        .map(|error| format!("{}: {}", error.name(), error.message()))
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

    /// #2692: the spelling TS's composite run key sees. `null`/absent keep
    /// their template spellings instead of folding to `""` (the old
    /// submit-side fold collapsed the two states); a too-deep array is an
    /// error, not a fold — the submit path rejects it before the key matters
    /// (`ts-prodkey-probe.out`, cell e: TS reports the same overflow from
    /// `structuredClone` after persisting, so the record is unreadable there
    /// too).
    #[test]
    fn template_key_spells_the_template_literal_form() {
        for (value, expected) in [
            (JsValue::Null, "null"),
            (JsValue::Undefined, "undefined"),
            (JsValue::from("session"), "session"),
            (JsValue::from(""), ""),
            (JsValue::Bool(true), "true"),
            (JsValue::Number(42.0), "42"),
            (parse_json("[1,null]").unwrap(), "1,"),
        ] {
            assert_eq!(
                template_key_checked(&value)
                    .expect("#2692 the spelling converts")
                    .to_utf8()
                    .unwrap(),
                expected,
                "#2692 {value:?}"
            );
        }
        assert_eq!(
            template_key_checked(&nested(MAX_ARRAY_DEPTH + 1)),
            Err(ToStringError::StackOverflow),
            "#2692 a too-deep array is an unreadable key, not a blank one"
        );
    }

    /// #2692: the raw-value key the router-shaped maps (sessions, in-flight
    /// turns, coordinator sinks, busy-gate pre-checks) index by — SameValueZero
    /// over the request's original value (`ts-prodkey-probe.out`, cells b–d:
    /// `null`/`42`/object keys never meet their string spellings, a fresh
    /// object never meets an equal-shaped one). Strings are identity; every
    /// other category is tagged, so the string `"null"` and JSON `null` get
    /// different keys while `-0`/`+0` share one, like SameValueZero.
    #[test]
    fn router_keys_separate_raw_categories_from_string_spellings() {
        let key = |value: &JsValue| {
            router_key(value)
                .expect("#2692 the raw key converts")
                .to_utf8()
                .unwrap()
        };
        assert_eq!(key(&JsValue::from("session")), "session");
        assert_eq!(key(&JsValue::from("null")), "null");
        assert_eq!(key(&JsValue::Null), "\0\0raw:null");
        assert_eq!(key(&JsValue::Undefined), "\0\0raw:undefined");
        assert_eq!(key(&JsValue::Bool(true)), "\0\0raw:true");
        assert_eq!(key(&JsValue::Bool(false)), "\0\0raw:false");
        assert_eq!(key(&JsValue::Number(42.0)), "\0\0raw:number:42");
        assert_eq!(key(&JsValue::Number(-0.0)), "\0\0raw:number:0");
        assert_eq!(key(&JsValue::Number(0.0)), "\0\0raw:number:0");
        // SameValueZero: distinct spellings stay distinct keys.
        assert_ne!(key(&JsValue::Null), key(&JsValue::from("null")));
        assert_ne!(key(&JsValue::Number(42.0)), key(&JsValue::from("42")));
        // Containers join their template spelling under the object tag.
        assert_eq!(key(&parse_json("[1,null]").unwrap()), "\0\0raw:object:1,");
        // A key whose spelling throws is an error the caller turns into a
        // rejection (submit) or a miss (read faces) — never a blank key.
        assert_eq!(
            router_key(&nested(MAX_ARRAY_DEPTH + 1)),
            Err(ToStringError::StackOverflow)
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

    /// #2301 fix-r2 review P1 #3: `Number(value)` throws on more than an own
    /// `toString` — arrays join (so the element's toPrimitive fails), nesting
    /// recurses, and the prototype chain a config merge installed is visible
    /// to the lookup; a too-deep array reports V8's RangeError instead.
    /// fix-r3 review P1 #2: a chain that terminates at `null`
    /// (`Object.create(null)`, the merge's `__proto__: null` arm) finds
    /// neither `valueOf` nor `toString` at all and throws the same TypeError;
    /// a non-object prototype value is ignored by JS, so it does not throw.
    #[test]
    fn number_conversion_error_covers_arrays_and_prototypes() {
        let json = |text: &str| parse_json(text).unwrap();
        // Clean shapes never throw (NaN at worst).
        assert_eq!(number_conversion_error(&json("[]")), None);
        assert_eq!(number_conversion_error(&json("[5]")), None);
        assert_eq!(number_conversion_error(&json("[1,2]")), None);
        assert_eq!(number_conversion_error(&json("{}")), None);
        assert_eq!(number_conversion_error(&json("5")), None);
        // The own `toString` (the fix-r1 shape), inside an array, and nested.
        let type_error = "TypeError: Cannot convert object to primitive value".to_owned();
        assert_eq!(
            number_conversion_error(&json(r#"{"toString": null}"#)),
            Some(type_error.clone())
        );
        assert_eq!(
            number_conversion_error(&json(r#"[{"toString": null}]"#)),
            Some(type_error.clone())
        );
        assert_eq!(
            number_conversion_error(&json(r#"[[{"toString": null}]]"#)),
            Some(type_error)
        );
        // The `__proto__` shape: the config merge installs the value as the
        // object's prototype (`config::merge`'s prototype arm), so the lookup
        // walks the chain.
        let mut port = pilotdeck_protocol::JsObject::new();
        port.set_prototype(json(r#"{"toString": null}"#));
        assert_eq!(
            number_conversion_error(&JsValue::Object(port)),
            Some("TypeError: Cannot convert object to primitive value".to_owned())
        );
        // The null-prototype family (fix-r3): a chain that terminates at
        // `null` — directly, or after an intermediate object — finds neither
        // coercion method and throws; an own non-callable `toString` on a
        // null-prototype object shadows nothing and throws as well.
        let null_proto = |own: &str| {
            let mut port = pilotdeck_protocol::JsObject::new();
            port.set_prototype(JsValue::Null);
            if !own.is_empty() {
                port.insert("toString".into(), json(own));
            }
            JsValue::Object(port)
        };
        assert_eq!(
            number_conversion_error(&null_proto("")),
            Some("TypeError: Cannot convert object to primitive value".to_owned())
        );
        assert_eq!(
            number_conversion_error(&null_proto("5")),
            Some("TypeError: Cannot convert object to primitive value".to_owned())
        );
        let mut middle = pilotdeck_protocol::JsObject::new();
        middle.set_prototype(JsValue::Null);
        let mut chained = pilotdeck_protocol::JsObject::new();
        chained.set_prototype(JsValue::Object(middle));
        assert_eq!(
            number_conversion_error(&JsValue::Object(chained)),
            Some("TypeError: Cannot convert object to primitive value".to_owned())
        );
        // A valueOf-only object with a null prototype: `valueOf` is present
        // but not callable, `toString` is nowhere — still the TypeError.
        let mut value_of_only = pilotdeck_protocol::JsObject::new();
        value_of_only.set_prototype(JsValue::Null);
        value_of_only.insert("valueOf".into(), JsValue::Null);
        assert_eq!(
            number_conversion_error(&JsValue::Object(value_of_only)),
            Some("TypeError: Cannot convert object to primitive value".to_owned())
        );
        // A non-object prototype value is ignored (JS keeps the implicit
        // Object.prototype), so neither of these throws.
        let mut ignored = pilotdeck_protocol::JsObject::new();
        ignored.set_prototype(JsValue::Number(5.0));
        assert_eq!(number_conversion_error(&JsValue::Object(ignored)), None);
        let mut ignored_array = pilotdeck_protocol::JsObject::new();
        ignored_array.set_prototype(JsValue::Array(Vec::new()));
        assert_eq!(
            number_conversion_error(&JsValue::Object(ignored_array)),
            None
        );
        // An own `valueOf: null` with the implicit prototype: the inherited
        // `toString` is still callable, so `Number()` is NaN, not a throw.
        assert_eq!(number_conversion_error(&json(r#"{"valueOf": null}"#)), None);
        // A too-deep array join reports V8's RangeError, not a Rust overflow.
        assert_eq!(
            number_conversion_error(&nested(MAX_ARRAY_DEPTH + 1)),
            Some("RangeError: Maximum call stack size exceeded".to_owned())
        );
        assert_eq!(number_conversion_error(&nested(MAX_ARRAY_DEPTH)), None);
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
