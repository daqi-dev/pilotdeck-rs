use pilotdeck_module2_host::{KernelClient, LcError, HOST_METHODS, dispatch::{dispatch, HostCall, HostError, HostExchange}, protocol::{decode_boundary_line, decode_line, Frame}};
use serde_json::{json, Value};
struct Echo;
impl HostExchange for Echo {
    fn call(&self, call: &HostCall) -> Result<Value, HostError> { Ok(call.params.clone()) }
}
impl KernelClient for Echo {
    fn call(&self, _: &str, params: Value) -> Result<Value, LcError> { Ok(params) }
}
fn main() {
    assert_eq!(HOST_METHODS.len(), 24);
    let raw = r#"{"kind":"request","messageId":"m","method":"module_call","runId":"r","operationId":"o","requestId":"q","module":"event","operation":"emit","payload":{"extension":null}}"#;
    let Frame::Request(request) = decode_line(raw).unwrap() else { panic!("request expected") };
    let Frame::Response(reply) = dispatch(&Echo, &request, || "response".into()) else { panic!("response expected") };
    assert_eq!(reply.in_reply_to, "m");
    assert_eq!(reply.payload, Some(json!({"extension":null})));
    assert_eq!(KernelClient::call(&Echo, "caller-owned-port", json!(true)).unwrap(), json!(true));
    let utf16 = raw.replace("null", "\"\\ud800\"");
    let value = decode_boundary_line(&utf16).unwrap();
    assert_eq!(value.get("payload").unwrap().get("extension").unwrap().as_string().unwrap().units(), &[0xd800]);
    assert_eq!(decode_line(&utf16).unwrap_err().code(), "UNREPRESENTABLE_FRAME");
    println!("Module2 external workspace: 24-method inventory, routing and lossless/interop boundary checked; no runtime authority supplied");
}
