//! Shared ECMAScript wire coercions.
pub(crate) use pilotdeck_protocol::coercion::*;
#[cfg(test)]
use pilotdeck_protocol::{JsString, JsValue};
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
