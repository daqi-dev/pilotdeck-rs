use pilotdeck_module2_host::{
    dispatch::{HostCall, HostError, HostExchange, dispatch},
    protocol::{Frame, decode_line, encode_frame},
};
use serde_json::{Value, json};
struct Echo;
impl HostExchange for Echo {
    fn call(&self, call: &HostCall) -> Result<Value, HostError> {
        Ok(call.params.clone())
    }
}
fn request(payload: Value, top: Option<Value>) -> pilotdeck_module2_host::protocol::RequestFrame {
    let mut v = json!({"kind":"request","messageId":"m","method":"module_call","runId":"r","operationId":"o","requestId":"q","module":"event","payload":payload});
    if let Some(top) = top {
        v["operation"] = top;
    }
    let Frame::Request(r) = decode_line(&v.to_string()).unwrap() else {
        panic!()
    };
    r
}
#[test]
fn payload_operation_is_authoritative_for_original_frames() {
    for top in [
        None,
        Some(json!("emit")),
        Some(json!("conflict")),
        Some(json!(3)),
    ] {
        let req = request(json!({"operation":"emit","extension":null}), top);
        let Frame::Response(reply) = dispatch(&Echo, &req, || "reply".into()) else {
            panic!("original payload-only call must route")
        };
        assert!(reply.ok);
        let wire: Value = serde_json::from_str(&encode_frame(&Frame::Response(reply))).unwrap();
        assert_eq!(wire["requestId"], "q");
        assert_eq!(wire["payload"], req.payload.unwrap());
    }
}
#[test]
fn driver_can_settle_each_rejected_request_without_a_stream_error() {
    for (payload, code) in [
        (json!({}), "HOST_CALL_INVALID"),
        (json!({"operation":3}), "HOST_CALL_INVALID"),
        (json!({"operation":""}), "HOST_CALL_INVALID"),
        (json!({"operation":"bogus"}), "HOST_METHOD_UNSUPPORTED"),
    ] {
        let req = request(payload, None);
        let reply = dispatch(&Echo, &req, || "reply".into());
        let Frame::Response(ref r) = reply else {
            panic!("request rejection must settle pending m, not stream error")
        };
        assert!(!r.ok);
        assert_eq!(r.in_reply_to, "m");
        let wire: Value = serde_json::from_str(&encode_frame(&reply)).unwrap();
        assert_eq!(wire["requestId"], "q");
        assert_eq!(wire["error"]["code"], code);
        assert!(!wire["error"]["message"].as_str().unwrap().is_empty());
    }
}
#[test]
fn dto_vocabulary_tracks_original_protocol_types_and_validator() {
    use pilotdeck_module2_host::protocol::{ModuleTarget as Dto, Retryability as DtoRetry};
    use pilotdeck_protocol::module::{ModuleTarget as Original, Retryability as Retry};
    // Exhaustive matches make additions to the original public enums a compile error.
    let target = |v| match v {
        Original::Model => Dto::Model,
        Original::Budget => Dto::Budget,
        Original::Turn => Dto::Turn,
        Original::Capability => Dto::Capability,
        Original::Permission => Dto::Permission,
        Original::Checkpoint => Dto::Checkpoint,
        Original::Context => Dto::Context,
        Original::Skills => Dto::Skills,
        Original::Knowledge => Dto::Knowledge,
        Original::Lifecycle => Dto::Lifecycle,
        Original::Event => Dto::Event,
    };
    for original in [
        Original::Model,
        Original::Budget,
        Original::Turn,
        Original::Capability,
        Original::Permission,
        Original::Checkpoint,
        Original::Context,
        Original::Skills,
        Original::Knowledge,
        Original::Lifecycle,
        Original::Event,
    ] {
        let mut req = request(json!({"operation":"emit"}), None);
        req.raw["module"] = json!(target(original).as_str());
        let Frame::Request(parsed) = decode_line(&req.raw.to_string()).unwrap() else {
            panic!()
        };
        assert_eq!(parsed.module, Some(target(original)));
    }
    let retry = |v| match v {
        Retry::Safe => DtoRetry::Safe,
        Retry::Unsafe => DtoRetry::Unsafe,
        Retry::RetryAfterStatus => DtoRetry::RetryAfterStatus,
    };
    for original in [Retry::Safe, Retry::Unsafe, Retry::RetryAfterStatus] {
        let raw = json!({"kind":"error","messageId":"e","code":"test","message":"detail","retryability":retry(original).as_str()});
        let Frame::Error(parsed) = decode_line(&raw.to_string()).unwrap() else {
            panic!()
        };
        assert_eq!(parsed.retryability, retry(original));
    }
    let mut req = request(json!({"operation":"emit"}), None).raw;
    req["module"] = json!("unregistered");
    assert!(decode_line(&req.to_string()).is_err());
    assert!(decode_line(&json!({"kind":"error","messageId":"e","code":"test","message":"detail","retryability":"unregistered"}).to_string()).is_err());
}
