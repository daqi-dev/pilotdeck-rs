//! Pinned exported default-factory consumer errors; independent actual91..104
//! observations supply counterexamples. These controlled exchanges aren't runtime proof.
use pilotdeck_agent::ports::*;
use pilotdeck_protocol::admission::CallEffect;
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use pilotdeck_transport::*;
fn json(raw: &str) -> JsValue {
    parse_json(raw).unwrap()
}
fn object(pairs: Vec<(&str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in pairs {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn route() -> Route {
    Route {
        provider: "p".into(),
        model: "m".into(),
    }
}
fn owner() -> ExecutionIdentity {
    ExecutionIdentity {
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "r".into(),
        operation_id: "o".into(),
        request_id: "owner-issued".into(),
        idempotency_key: None,
        module_instance_id: "i".into(),
        connection_generation: "g".into(),
    }
}
struct Exchange {
    response: JsValue,
    calls: usize,
}
impl HostExchange for Exchange {
    fn exchange(
        &mut self,
        _: &CallControl,
        _: &'static str,
        _: &'static str,
        _: CallEffect,
        _: JsObject,
    ) -> PortResult<HostExchangeResponse> {
        self.calls += 1;
        Ok(HostExchangeResponse {
            identity: owner(),
            cancelled_before_response: false,
            boundary: BoundarySnapshot::capture(&self.response),
        })
    }
}
fn response(payload: JsValue) -> JsValue {
    object(vec![
        ("kind", "response".into()),
        ("messageId", "m".into()),
        ("inReplyTo", "r".into()),
        ("ok", JsValue::Bool(true)),
        ("payload", payload),
    ])
}
fn host(raw: JsValue) -> RemoteHost<Exchange> {
    RemoteHost::new(
        Exchange {
            response: raw,
            calls: 0,
        },
        AdvertisedMethods {
            modules: vec![
                (
                    "model".into(),
                    vec!["get_metadata".into(), "prepare".into()],
                ),
                ("capability".into(), vec!["list_tools".into()]),
            ],
        },
    )
}
fn call(raw: JsValue, op: HostCall) -> PortResult<HostReply> {
    let mut h = host(raw);
    let result = h.call(
        &CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        },
        op,
    );
    assert_eq!(h.into_exchange().calls, 1);
    result
}
fn metadata(raw: JsValue) -> PortResult<HostReply> {
    call(
        response(object(vec![("metadata", raw)])),
        HostCall::ModelMetadata { route: route() },
    )
}
fn message(result: PortResult<HostReply>) -> JsString {
    match result {
        Err(PortError::InvalidResponse { message }) => message,
        other => panic!("expected source invalid response, got {other:?}"),
    }
}
fn catalog(raw: JsValue) -> PortResult<HostReply> {
    call(response(object(vec![("tools", raw)])), HostCall::ListTools)
}
fn meta(field: &str, value: JsValue) -> JsValue {
    object(vec![
        ("provider", "p".into()),
        ("model", "m".into()),
        (field, value),
    ])
}
#[test]
fn independent_091_092_protocol_messages_preserve_exact_consumer_error() {
    for value in ["future".into(), JsValue::Null] {
        assert!(
            message(metadata(meta("protocol", value)))
                .equals_str("Host model metadata response contains an invalid protocol.")
        );
    }
}
#[test]
fn independent_096_098_cache_capability_messages_do_not_convert_null_to_missing() {
    for value in [JsValue::Null, JsValue::Number(1.0)] {
        assert!(
            message(metadata(meta("supportsPromptCache", value))).equals_str(
                "Host model metadata response contains an invalid prompt-cache capability."
            )
        );
    }
}
#[test]
fn independent_101_102_duplicate_and_empty_catalog_share_original_descriptor_error() {
    for raw in [
        json(r#"[{"name":"x"},{"name":"x"}]"#),
        json(r#"[{"name":""}]"#),
    ] {
        assert!(message(catalog(raw)).equals_str(
            "Host tool catalog response contains an invalid or duplicate tool descriptor."
        ));
    }
}
#[test]
fn independent_103_104_catalog_container_has_specific_array_error() {
    for raw in [JsValue::Null, json("{}")] {
        assert!(
            message(catalog(raw))
                .equals_str("Host tool catalog response must contain a tools array.")
        );
    }
}
#[test]
fn metadata_order_route_then_scalar_limits_then_nested_limits_then_protocol_then_cache() {
    assert!(message(metadata(json(r#"{"provider":"","model":"m","maxContextTokens":null,"protocol":"bad","supportsPromptCache":null}"#))).equals_str("Host model metadata response must include provider and model."));
    assert!(message(metadata(json(r#"{"provider":"p","model":"m","maxContextTokens":null,"maxOutputTokens":null,"protocol":"bad"}"#))).equals_str("Host model metadata response contains an invalid maxContextTokens."));
    assert!(message(metadata(json(r#"{"provider":"p","model":"m","maxContextTokens":1,"maxOutputTokens":null,"tokenLimits":{},"protocol":"bad"}"#))).equals_str("Host model metadata response contains an invalid maxOutputTokens."));
    assert!(message(metadata(json(r#"{"provider":"p","model":"m","tokenLimits":{},"protocol":"bad","supportsPromptCache":null}"#))).equals_str("Host model metadata response contains an invalid tokenLimits.maxContextTokens."));
    assert!(message(metadata(json(r#"{"provider":"p","model":"m","tokenLimits":{"maxContextTokens":1,"maxOutputTokens":null},"protocol":"bad"}"#))).equals_str("Host model metadata response contains an invalid tokenLimits.maxOutputTokens."));
    assert!(
        message(metadata(json(
            r#"{"provider":"p","model":"m","protocol":"bad","supportsPromptCache":null}"#
        )))
        .equals_str("Host model metadata response contains an invalid protocol.")
    );
}
#[test]
fn missing_metadata_and_missing_tools_have_source_errors_not_generic_field_errors() {
    assert!(
        message(call(
            response(json("{}")),
            HostCall::ModelMetadata { route: route() }
        ))
        .equals_str("Host model metadata response must include provider and model.")
    );
    assert!(
        message(call(response(json("{}")), HostCall::ListTools))
            .equals_str("Host tool catalog response must contain a tools array.")
    );
}
#[test]
fn catalog_object_available_and_unicode_names_are_source_valid_with_raw_retained() {
    let raw = json(
        r#"{"available":[{"name":"\ud800","description":"","input_schema":{"type":"object"},"readOnly":1,"requiredRuntimeCapabilities":["user_interaction","user_interaction"]}]}"#,
    );
    let HostReply::Tools(tools) = catalog(raw).unwrap() else {
        panic!()
    };
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].view.name.units(), [0xd800]);
    assert!(tools[0].view.description.equals_str("Host capability"));
    assert!(!tools[0].view.read_only);
    assert_eq!(
        tools[0]
            .view
            .required_runtime_capabilities
            .as_ref()
            .unwrap(),
        &[JsString::from("user_interaction")]
    );
    assert_eq!(
        tools[0]
            .boundary
            .value()
            .get("readOnly")
            .unwrap()
            .as_number(),
        Some(1.0)
    );
    for value in [
        JsValue::Null,
        JsValue::Number(1.0),
        JsValue::Array(vec![]),
        json("{}"),
        json(r#"{"name":null}"#),
    ] {
        assert!(message(catalog(JsValue::Array(vec![value]))).equals_str(
            "Host tool catalog response contains an invalid or duplicate tool descriptor."
        ));
    }
}
#[test]
fn metadata_own_undefined_optional_and_null_token_limits_preserve_original_acceptance() {
    let raw = object(vec![
        ("provider", "p".into()),
        ("model", "m".into()),
        ("protocol", JsValue::Undefined),
        ("supportsPromptCache", JsValue::Undefined),
        ("maxContextTokens", JsValue::Undefined),
        ("tokenLimits", JsValue::Null),
        ("extension", JsValue::Number(7.0)),
    ]);
    let HostReply::ModelMetadata(value) = metadata(raw.clone()).unwrap() else {
        panic!()
    };
    assert!(value.view.protocol.is_none());
    assert!(value.view.token_limits.is_none());
    assert_eq!(value.boundary.value(), &raw);
    for raw in [
        json(r#"{"provider":"p","model":"m","tokenLimits":[]}"#),
        json(r#"{"provider":"p","model":"m","tokenLimits":7}"#),
    ] {
        assert!(metadata(raw).is_ok());
    }
}
#[test]
fn source_safe_positive_metadata_limits_do_not_admit_nan_infinity_fraction_or_unsafe() {
    for value in [
        0.0,
        -0.0,
        -1.0,
        0.5,
        f64::NAN,
        f64::INFINITY,
        9007199254740992.0,
    ] {
        assert!(
            message(metadata(meta("maxContextTokens", JsValue::Number(value))))
                .equals_str("Host model metadata response contains an invalid maxContextTokens.")
        );
    }
    assert!(
        metadata(meta(
            "maxContextTokens",
            JsValue::Number(9007199254740991.0)
        ))
        .is_ok()
    );
}
#[test]
fn route_mismatch_is_checked_after_source_snapshot_validation() {
    assert!(
        message(metadata(json(
            r#"{"provider":"wrong","model":"m","protocol":"bad"}"#
        )))
        .equals_str("Host model metadata response contains an invalid protocol.")
    );
    assert!(
        message(metadata(json(r#"{"provider":"wrong","model":"m"}"#)))
            .equals_str("Host model metadata response does not match the requested route.")
    );
}
#[test]
fn module_failure_and_unknown_outcome_still_precede_success_shape() {
    let failure = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"r","ok":false,"code":"HOST_FAILURE","error":{"message":"original failure"},"payload":{"metadata":{"protocol":"future"}}}"#,
    );
    let Err(PortError::HostFailure { code, message, .. }) =
        call(failure, HostCall::ModelMetadata { route: route() })
    else {
        panic!()
    };
    assert!(code.equals_str("HOST_FAILURE"));
    assert!(message.equals_str("original failure"));
    let unknown = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"r","ok":true,"outcome":"result_unknown","payload":{"tools":null}}"#,
    );
    let Err(PortError::AcceptedUnknown { identity, .. }) = call(unknown, HostCall::ListTools)
    else {
        panic!()
    };
    assert_eq!(*identity, owner());
}
