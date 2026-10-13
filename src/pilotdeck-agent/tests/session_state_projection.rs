//! Source-derived supplementary projection checks. Independent exported oracle is separate.
use pilotdeck_agent::ports::PortError;
use pilotdeck_agent::session_state::*;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
fn json(raw: &str) -> JsValue {
    parse_json(raw).unwrap()
}
fn object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn err(result: Result<Option<AgentFileState>, PortError>) -> JsString {
    match result {
        Err(PortError::InvalidResponse { message }) => message,
        other => panic!("unexpected {other:?}"),
    }
}
fn model_err(v: &JsValue) -> JsString {
    match parse_model_state(v) {
        Err(PortError::InvalidResponse { message }) => message,
        other => panic!("unexpected {other:?}"),
    }
}
#[test]
fn nullish_omits_state_and_empty_object_restores_empty_state() {
    for v in [JsValue::Undefined, JsValue::Null] {
        assert!(parse_file_state(&v).unwrap().is_none());
        assert!(parse_model_state(&v).unwrap().is_none());
    }
    let state = parse_file_state(&json("{}")).unwrap().unwrap();
    let raw = snapshot_file_state(&state);
    let keys: Vec<_> = raw
        .value()
        .as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(k, _)| k.to_utf8().unwrap())
        .collect();
    assert_eq!(
        keys,
        ["allowedReadFiles", "readFileState", "writeSnapshots"]
    );
    assert!(
        snapshot_model_state(&parse_model_state(&json("{}")).unwrap().unwrap())
            .value()
            .as_object()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn allowed_validation_precedes_file_maps_and_set_retains_first_slot() {
    let v = json(r#"{"allowedReadFiles":["x",7],"readFileState":null}"#);
    assert!(err(parse_file_state(&v)).equals_str("Invalid sidecar seedState.allowedReadFiles."));
    let state = parse_file_state(&json(r#"{"allowedReadFiles":["x","y","x",""]}"#))
        .unwrap()
        .unwrap();
    assert_eq!(
        state.allowed_read_files,
        vec!["x".into(), "y".into(), "".into()]
    );
}
#[test]
fn own_undefined_optional_but_null_rejected() {
    let state = parse_file_state(&object(vec![
        ("allowedReadFiles", JsValue::Undefined),
        ("readFileState", JsValue::Undefined),
        ("writeSnapshots", JsValue::Undefined),
    ]))
    .unwrap()
    .unwrap();
    assert!(state.read_file_state.is_empty());
    assert!(
        err(parse_file_state(&json(r#"{"allowedReadFiles":null}"#)))
            .equals_str("Invalid sidecar seedState.allowedReadFiles.")
    );
    assert!(
        model_err(&json(r#"{"tokenCalibration":null}"#))
            .equals_str("Invalid sidecar modelState.tokenCalibration: expected an array.")
    );
}
#[test]
fn seed_typeof_numbers_keep_nan_infinity_signed_zero_and_ignore_other_optionals() {
    let entry = object(vec![
        ("mtimeMs", JsValue::Number(f64::NAN)),
        ("kind", "text".into()),
        ("offset", JsValue::Number(f64::NEG_INFINITY)),
        ("limit", JsValue::Number(-0.0)),
        ("pages", JsValue::Null),
    ]);
    let state = parse_file_state(&object(vec![("readFileState", object(vec![("x", entry)]))]))
        .unwrap()
        .unwrap();
    let entry = &state.read_file_state[0].1;
    assert!(entry.mtime_ms.is_nan());
    assert_eq!(entry.offset, Some(f64::NEG_INFINITY));
    assert_eq!(entry.limit.unwrap().to_bits(), (-0.0f64).to_bits());
    assert!(entry.pages.is_none());
    let raw = snapshot_file_state(&state);
    let entry = raw.value().get("readFileState").unwrap().get("x").unwrap();
    assert!(entry.get("mtimeMs").unwrap().as_number().unwrap().is_nan());
    assert!(entry.get("pages").is_none());
}
#[test]
fn object_entries_numeric_order_and_proto_owned_key() {
    let v = json(
        r#"{"readFileState":{"10":{"mtimeMs":1,"kind":"text"},"2":{"mtimeMs":2,"kind":"image"},"01":{"mtimeMs":3,"kind":"pdf"},"4294967295":{"mtimeMs":4,"kind":"notebook"},"__proto__":{"mtimeMs":5,"kind":"text"}}}"#,
    );
    let state = parse_file_state(&v).unwrap().unwrap();
    assert_eq!(
        state
            .read_file_state
            .iter()
            .map(|(k, _)| k.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        ["2", "10", "01", "4294967295", "__proto__"]
    );
    assert!(
        snapshot_file_state(&state)
            .value()
            .get("readFileState")
            .unwrap()
            .get("__proto__")
            .is_some()
    );
}
#[test]
fn map_validation_uses_js_entries_order_before_write_validation() {
    let v = json(r#"{"readFileState":{"z":{},"2":{}},"writeSnapshots":null}"#);
    assert!(err(parse_file_state(&v)).equals_str("Invalid readFileState entry: 2."));
}
#[test]
fn invalid_path_error_keeps_wtf16() {
    let key = JsString::from_utf16(vec![0xd800]);
    let mut reads = JsObject::new();
    reads.insert(key.clone(), JsValue::Null);
    let actual = err(parse_file_state(&object(vec![(
        "readFileState",
        JsValue::Object(reads),
    )])));
    let mut expected: Vec<u16> = "Invalid readFileState entry: ".encode_utf16().collect();
    expected.push(0xd800);
    expected.push(46);
    assert_eq!(actual.units(), expected);
}
#[test]
fn write_entry_required_strings_allow_empty_and_optional_numbers_are_not_finite_guarded() {
    let v = json(
        r#"{"writeSnapshots":{"x":{"absolutePath":"","mtimeMs":-1,"contentHash":"","offset":-2,"limit":null}}}"#,
    );
    let state = parse_file_state(&v).unwrap().unwrap();
    let entry = &state.write_snapshots[0].1;
    assert!(entry.absolute_path.is_empty());
    assert!(entry.content_hash.is_empty());
    assert_eq!(entry.offset, Some(-2.0));
    assert!(entry.limit.is_none());
    assert_eq!(
        snapshot_file_state(&state)
            .value()
            .get("writeSnapshots")
            .unwrap()
            .get("x")
            .unwrap()
            .as_object()
            .unwrap()
            .entries()
            .iter()
            .map(|(k, _)| k.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        ["absolutePath", "mtimeMs", "contentHash", "offset"]
    );
}
#[test]
fn file_top_level_and_map_types_reject_source_invalid_shapes() {
    for v in [JsValue::Bool(true), json("[]"), JsValue::Number(1.0)] {
        assert!(
            err(parse_file_state(&v)).equals_str("Invalid sidecar seedState: expected an object.")
        );
    }
    assert!(
        err(parse_file_state(&json(r#"{"readFileState":[]}"#)))
            .equals_str("Invalid sidecar seedState.readFileState.")
    );
    assert!(
        err(parse_file_state(&json(r#"{"writeSnapshots":null}"#)))
            .equals_str("Invalid sidecar seedState.writeSnapshots.")
    );
    for v in [
        r#"{"writeSnapshots":{"x":{"absolutePath":7,"mtimeMs":1,"contentHash":""}}}"#,
        r#"{"readFileState":{"x":{"mtimeMs":"1","kind":"text"}}}"#,
        r#"{"readFileState":{"x":{"mtimeMs":1,"kind":"unknown"}}}"#,
    ] {
        assert!(parse_file_state(&json(v)).is_err());
    }
}
#[test]
fn calibration_validation_precedes_caps_and_fields_are_ordered() {
    assert!(
        model_err(&json(r#"{"tokenCalibration":[{}],"tokenCaps":null}"#))
            .equals_str("Invalid sidecar modelState calibration provider.")
    );
    assert!(model_err(&json(r#"{"tokenCalibration":[{"provider":"p","model":"m","actualInputTokens":0}],"tokenCaps":null}"#)).equals_str("Invalid sidecar modelState calibration actualInputTokens."));
    assert!(
        model_err(&json(r#"{"tokenCalibration":[null]}"#))
            .equals_str("Invalid sidecar modelState calibration entry.")
    );
}
#[test]
fn cap_numeric_validation_precedes_provider_and_required_persistent_check() {
    assert!(
        model_err(&json(
            r#"{"tokenCaps":[{"provider":"","maxContextTokens":null,"hardMaxOutputTokens":-1}]}"#
        ))
        .equals_str("Invalid sidecar modelState cap maxContextTokens.")
    );
    assert!(
        model_err(&json(r#"{"tokenCaps":[{}]}"#))
            .equals_str("Invalid sidecar modelState cap entry without a persistent cap.")
    );
    assert!(
        model_err(&json(r#"{"tokenCaps":[{"maxContextTokens":1}]}"#))
            .equals_str("Invalid sidecar modelState cap provider.")
    );
    assert!(
        model_err(&json(r#"{"tokenCaps":[[]]}"#))
            .equals_str("Invalid sidecar modelState cap entry.")
    );
}
#[test]
fn positive_finite_domain_rejects_all_nonpositive_and_nonfinite_numbers() {
    for number in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let v = object(vec![(
            "tokenCaps",
            JsValue::Array(vec![object(vec![
                ("provider", "p".into()),
                ("model", "m".into()),
                ("hardMaxOutputTokens", JsValue::Number(number)),
            ])]),
        )]);
        assert!(model_err(&v).equals_str("Invalid sidecar modelState cap hardMaxOutputTokens."));
    }
    let v = json(r#"{"tokenCaps":[{"provider":" ","model":" ","maxContextTokens":0.5}]}"#);
    assert_eq!(
        parse_model_state(&v).unwrap().unwrap().token_caps[0].max_context_tokens,
        Some(0.5)
    );
}
#[test]
fn joined_nul_collisions_replace_value_keep_first_slot_and_snapshot_source_split() {
    let v = json(
        r#"{"tokenCalibration":[{"provider":"p\u0000x","model":"y","actualInputTokens":1,"estimatedInputTokens":2},{"provider":"other","model":"route","actualInputTokens":3,"estimatedInputTokens":4},{"provider":"p","model":"x\u0000y","actualInputTokens":5,"estimatedInputTokens":6}],"tokenCaps":[{"provider":"p\u0000x","model":"y","maxContextTokens":8},{"provider":"other","model":"route","maxContextTokens":9},{"provider":"p","model":"x\u0000y","hardMaxOutputTokens":10}]}"#,
    );
    let state = parse_model_state(&v).unwrap().unwrap();
    assert_eq!(state.token_calibration.len(), 2);
    assert_eq!(state.token_caps.len(), 2);
    assert_eq!(state.token_calibration[0].actual_input_tokens, 5.0);
    assert!(state.token_calibration[0].route.model.equals_str("x\0y"));
    assert!(state.token_caps[0].max_context_tokens.is_none());
    assert_eq!(state.token_caps[0].hard_max_output_tokens, Some(10.0));
    let raw = snapshot_model_state(&state);
    let JsValue::Array(caps) = raw.value().get("tokenCaps").unwrap() else {
        panic!()
    };
    assert!(
        caps[0]
            .get("provider")
            .unwrap()
            .as_string()
            .unwrap()
            .equals_str("p")
    );
    assert!(
        caps[0]
            .get("model")
            .unwrap()
            .as_string()
            .unwrap()
            .equals_str("x")
    );
    let JsValue::Array(cal) = raw.value().get("tokenCalibration").unwrap() else {
        panic!()
    };
    assert!(
        cal[0]
            .get("model")
            .unwrap()
            .as_string()
            .unwrap()
            .equals_str("x\0y")
    );
}
#[test]
fn cap_snapshot_omits_empty_split_segments_and_empty_optional_arrays() {
    let state = parse_model_state(&json(
        r#"{"tokenCaps":[{"provider":"p","model":"\u0000m","maxContextTokens":1}]}"#,
    ))
    .unwrap()
    .unwrap();
    assert!(
        snapshot_model_state(&state)
            .value()
            .as_object()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn wtf16_route_is_nonempty_and_preserved_and_unknown_fields_are_projected_out() {
    let v = json(
        r#"{"tokenCalibration":[{"provider":"\ud800","model":"m","actualInputTokens":1,"estimatedInputTokens":2,"extra":null}],"ignored":7}"#,
    );
    let state = parse_model_state(&v).unwrap().unwrap();
    let raw = snapshot_model_state(&state);
    let JsValue::Array(cal) = raw.value().get("tokenCalibration").unwrap() else {
        panic!()
    };
    assert_eq!(
        cal[0].get("provider").unwrap().as_string().unwrap().units(),
        [0xd800]
    );
    assert!(cal[0].get("extra").is_none());
}
#[test]
fn snapshot_is_detached_from_later_typed_mutation() {
    let mut state = parse_file_state(&json(r#"{"allowedReadFiles":["before"]}"#))
        .unwrap()
        .unwrap();
    let raw = snapshot_file_state(&state);
    state.allowed_read_files[0] = "after".into();
    let JsValue::Array(paths) = raw.value().get("allowedReadFiles").unwrap() else {
        panic!()
    };
    assert!(paths[0].as_string().unwrap().equals_str("before"));
}

#[test]
fn independent_factory_twenty_invalid_seed_model_counterexamples() {
    // Independent observed factory-0428-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("false")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0429-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("true")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0430-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("0")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0431-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("1")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0432-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("-1")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0433-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("2.5")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0434-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("\"\"")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0435-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("\" x\\ud800\\u4e2d \"")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0436-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json("[]")))
            .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0437-payload-seedState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        err(parse_file_state(&json(
            "[null,\"a\",{\"__pd_undefined__\":true}]"
        )))
        .equals_str("Invalid sidecar seedState: expected an object.")
    );
    // Independent observed factory-0445-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        model_err(&json("false")).equals_str("Invalid sidecar modelState: expected an object.")
    );
    // Independent observed factory-0446-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(model_err(&json("true")).equals_str("Invalid sidecar modelState: expected an object."));
    // Independent observed factory-0447-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(model_err(&json("0")).equals_str("Invalid sidecar modelState: expected an object."));
    // Independent observed factory-0448-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(model_err(&json("1")).equals_str("Invalid sidecar modelState: expected an object."));
    // Independent observed factory-0449-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(model_err(&json("-1")).equals_str("Invalid sidecar modelState: expected an object."));
    // Independent observed factory-0450-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(model_err(&json("2.5")).equals_str("Invalid sidecar modelState: expected an object."));
    // Independent observed factory-0451-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(model_err(&json("\"\"")).equals_str("Invalid sidecar modelState: expected an object."));
    // Independent observed factory-0452-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        model_err(&json("\" x\\ud800\\u4e2d \""))
            .equals_str("Invalid sidecar modelState: expected an object.")
    );
    // Independent observed factory-0453-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(model_err(&json("[]")).equals_str("Invalid sidecar modelState: expected an object."));
    // Independent observed factory-0454-payload-modelState; native markers inside arrays need no conversion for this top-level rejection.
    assert!(
        model_err(&json("[null,\"a\",{\"__pd_undefined__\":true}]"))
            .equals_str("Invalid sidecar modelState: expected an object.")
    );
}
