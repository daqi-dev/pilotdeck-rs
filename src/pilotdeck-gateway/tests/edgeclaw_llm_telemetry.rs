//! #868: dedicated panic messages for telemetry mutations; #763 owns the oracle.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::llm::transport::{LlmError, LlmMemoryExtractor, LlmRuntime, StructuredInput},
    model::http::{ModelHttpTransport, TransportFuture},
};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).expect("#868 fixture JSON parses")
}
fn json_value(value: &JsValue) -> Value {
    serde_json::from_str(
        &stringify_json(value)
            .expect("#868 event serializes")
            .expect("#868 event is defined"),
    )
    .expect("#868 event JSON parses")
}
#[derive(Default)]
struct Runtime {
    auth: String,
    events: Mutex<Vec<Value>>,
}
impl LlmRuntime for Runtime {
    fn env(&self, _: &str) -> Option<String> {
        None
    }
    fn track_feature_loop_stage(&self, event: JsValue) {
        self.events
            .lock()
            .expect("#868 stage lock")
            .push(json_value(&event));
    }
    fn track_error(&self, error: &LlmError, event: JsValue) {
        let mut event = json_value(&event);
        event["error"] = json!(error.message.to_utf8().expect("#868 error text"));
        self.events.lock().expect("#868 error lock").push(event);
    }
    fn resolve_auth<'a>(
        &'a self,
        _: &'a JsString,
        _: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            if self.auth.is_empty() {
                return Ok(None);
            }
            self.events
                .lock()
                .expect("#868 auth lock")
                .push(json!({"auth":"called"}));
            if self.auth == "reject" {
                Err(LlmError::new("auth failed"))
            } else {
                Ok(Some(js(json!({"apiKey":"resolved-key"}))))
            }
        })
    }
    fn sleep(&self, _: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}
struct Transport {
    steps: Vec<Value>,
    calls: AtomicUsize,
}
impl ModelHttpTransport for Transport {
    fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let step = self
            .steps
            .get(index)
            .or_else(|| self.steps.last())
            .expect("#868 response step");
        let body = Full::new(Bytes::from(
            step["body"].as_str().expect("#868 body text").to_owned(),
        ))
        .map_err(|never| match never {})
        .boxed();
        TransportFuture::ready(Ok(Response::builder()
            .status(step["status"].as_u64().expect("#868 response status") as u16)
            .body(body)
            .expect("#868 response builds")))
    }
}
async fn replay(names: &[&str], assertion: &str) {
    // The full #763 oracle contains lone UTF-16 surrogates; only the selected
    // telemetry cells below are UTF-8 and can be converted to serde_json.
    let oracle = parse_json(include_str!("fixtures/edgeclaw-llm/oracle.json"))
        .expect("#868 source oracle parses");
    assert_eq!(
        oracle.get("nodeVersion"),
        Some(&JsValue::from("v22.22.0")),
        "#868 pinned Node oracle"
    );
    let Some(JsValue::Array(cases)) = oracle.get("cases") else {
        panic!("#868 oracle cases");
    };
    for name in names {
        let case = json_value(
            cases
                .iter()
                .find(|c| c.get("spec").and_then(|s| s.get("name")) == Some(&JsValue::from(*name)))
                .expect("#868 named oracle case exists"),
        );
        let spec = &case["spec"];
        let runtime = Arc::new(Runtime {
            auth: spec["auth"].as_str().unwrap_or("").into(),
            ..Runtime::default()
        });
        let transport = Arc::new(Transport {
            steps: spec["steps"].as_array().expect("#868 steps").clone(),
            calls: AtomicUsize::new(0),
        });
        let mut provider = json!({"api":spec["api"],"baseUrl":"http://fixture.invalid/v1","apiKey":"fixture-key","models":[{"id":"m"}]});
        if let Some(missing) = spec["missing"].as_str() {
            provider
                .as_object_mut()
                .expect("#868 provider object")
                .remove(missing);
        }
        let config = js(
            json!({"agents":{"defaults":{"model":{"primary":"p/m"}}},"models":{"providers":{"p":provider}}}),
        );
        let extractor = LlmMemoryExtractor::new(config, transport.clone(), runtime.clone());
        let input = StructuredInput {
            system_prompt: "system 中\n\"quoted\"".into(),
            user_prompt: "user 😀".into(),
            agent_id: None,
            request_label: spec["label"].as_str().unwrap_or("recall fixture").into(),
            timeout_ms: None,
        };
        let result = extractor
            .call_structured_json_with_debug(
                &input,
                |raw| {
                    if spec["parse"] == "reject" {
                        Err(LlmError::new("parse rejected"))
                    } else {
                        Ok(JsValue::String(raw.clone()))
                    }
                },
                |_| {},
            )
            .await;
        let events = runtime
            .events
            .lock()
            .expect("#868 observed events lock")
            .clone();
        let events: Value = serde_json::from_str(
            &serde_json::to_string(&events)
                .expect("#868 events encode")
                .replace("http://fixture.invalid", "$ORIGIN"),
        )
        .expect("#868 normalized events decode");
        // Compare events first: an extract failure must not hide a missing response event.
        assert_eq!(events, case["expected"]["events"], "{assertion}: {name}");
        let actual = match result {
            Ok(value) => json!({"ok":json_value(&value)}),
            Err(error) => {
                let mut value = json!({"error":error.message.to_utf8().expect("#868 result error text"),"name":error.name});
                if let Some(status) = error.status {
                    value["status"] = json!(status);
                }
                value
            }
        };
        assert_eq!(actual, case["expected"]["result"], "#868 result for {name}");
        assert_eq!(
            json!(transport.calls.load(Ordering::SeqCst)),
            case["expected"]["calls"],
            "#868 calls for {name}"
        );
    }
}
#[tokio::test]
async fn request_precedes_auth() {
    replay(
        &[
            "auth-success",
            "auth-reject",
            "missing-apiKey",
            "missing-baseUrl",
        ],
        "#868 request precedes auth",
    )
    .await;
}
#[tokio::test]
async fn response_precedes_extract_without_tracking_parse_failures() {
    replay(
        &[
            "extract-chat-0",
            "extract-responses-0",
            "extract-anthropic-0",
            "extract-google-0",
            "json-2",
            "debug-reject",
        ],
        "#868 response precedes extract and excludes parse failures",
    )
    .await;
}
#[tokio::test]
async fn six_phase_labels() {
    replay(
        &[
            "telemetry-phase-retrieval",
            "telemetry-phase-DREAM",
            "telemetry-phase-rewrite",
            "telemetry-phase-capture",
            "telemetry-phase-extract",
            "telemetry-phase-index",
        ],
        "#868 all six phase labels",
    )
    .await;
}
#[tokio::test]
async fn only_final_request_failure_is_tracked() {
    replay(
        &[
            "chat-attempt-0-event-1",
            "responses-attempt-0-event-1",
            "chat-attempt-1-event-0",
            "responses-attempt-2-event-0",
        ],
        "#868 only final request failure is tracked",
    )
    .await;
}
