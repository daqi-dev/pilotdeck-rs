//! Shared ECMAScript wire coercions.
#[cfg(test)]
use pilotdeck_protocol::JsValue;
pub(crate) use pilotdeck_protocol::coercion::*;
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
