use pilotdeck_module2_host::{
    HOST_METHODS,
    dispatch::{HostCall, HostError, HostExchange, dispatch},
    protocol::{Frame, decode_boundary_line, decode_line, encode_frame},
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Echo(AtomicUsize);
impl HostExchange for Echo {
    fn call(&self, call: &HostCall) -> Result<Value, HostError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"module":call.module,"operation":call.method,"original":call.params}))
    }
}
#[test]
fn every_public_host_method_routes_once_and_preserves_correlation_and_payload() {
    let exchange = Echo::default();
    assert_eq!(HOST_METHODS.len(), 24);
    for (index, method) in HOST_METHODS.iter().enumerate() {
        let id = format!("request-{index}");
        let payload =
            json!({"operation":method.method,"nullable":null,"extension":{"value":[1,"a"]}});
        let raw = json!({"kind":"request","messageId":id,"method":"module_call","runId":"original-run","operationId":"original-operation","requestId":"original-request","module":method.module,"operation":method.method,"payload":payload}).to_string();
        let Frame::Request(request) = decode_line(&raw).unwrap() else {
            panic!("request expected")
        };
        let response = dispatch(&exchange, &request, || format!("response-{index}"));
        let encoded = encode_frame(&response);
        decode_boundary_line(&encoded).unwrap();
        let Frame::Response(response) = response else {
            panic!("response expected")
        };
        assert_eq!(response.in_reply_to, id);
        assert!(response.ok);
        assert_eq!(
            response.payload.unwrap(),
            json!({"module":method.module,"operation":method.method,"original":payload})
        );
        assert_eq!(exchange.0.load(Ordering::SeqCst), index + 1);
    }
}
#[test]
fn validation_errors_are_owned_by_the_original_protocol() {
    let cases = [
        "{",
        "null",
        r#"{"kind":"bogus"}"#,
        r#"{"kind":"request","messageId":"m","method":"execute","payload":{}}"#,
        r#"{"kind":"response","messageId":"m","inReplyTo":"q","ok":"true"}"#,
    ];
    for raw in cases {
        let expected = pilotdeck_protocol::parse_json(raw).ok().map(|value| {
            pilotdeck_protocol::wire::EnvelopeRef::decode(&value)
                .unwrap_err()
                .code
        });
        let observed = decode_boundary_line(raw).unwrap_err();
        assert_eq!(observed.code(), expected.unwrap_or("INVALID_JSON"), "{raw}");
    }
}
#[test]
fn unknown_operation_and_lifecycle_cannot_invoke_the_exchange() {
    let exchange = Echo::default();
    for raw in [
        r#"{"kind":"request","messageId":"m","method":"module_call","runId":"r","operationId":"o","requestId":"q","module":"model","payload":{"operation":"not-installed"}}"#,
        r#"{"kind":"request","messageId":"m","method":"hello","payload":{}}"#,
    ] {
        let Frame::Request(request) = decode_line(raw).unwrap() else {
            panic!("request expected")
        };
        let response = dispatch(&exchange, &request, || "response".into());
        assert_eq!(exchange.0.load(Ordering::SeqCst), 0);
        decode_boundary_line(&encode_frame(&response)).unwrap();
        match response {
            Frame::Response(reply) => {
                assert_eq!(reply.in_reply_to, "m");
                assert!(!reply.ok);
                assert_eq!(
                    reply.code.as_deref(),
                    Some(if request.method == "module_call" {
                        "HOST_METHOD_UNSUPPORTED"
                    } else {
                        "HOST_LIFECYCLE_UNSUPPORTED"
                    })
                );
            }
            Frame::Error(error) => {
                assert_eq!(error.message_id, "response");
                assert_eq!(error.code, "HOST_METHOD_UNSUPPORTED");
            }
            other => panic!("refusal expected: {other:?}"),
        }
    }
}
