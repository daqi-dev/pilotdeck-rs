//! Replays the real TS recordings for the Gateway user-dialog HTTP store.
//!
//! TS source: `resources/PilotDeck/src/gateway/user-dialog/HttpGatewayUserDialogStore.ts`
//! at `cc7c4e6`. `tests/fixtures/user-dialog-http-store/generate.mts` drives the
//! real TS client, handler and loopback server; every expected byte below comes
//! from `oracle.json` (generated twice byte-identically). The Rust side only
//! feeds the recorded inputs back through its own implementation.
//!
//! Direction coverage:
//! - `client_requests_*`: TS client -> wire request; Rust client must emit the
//!   same URL, headers and body bytes, and decode the TS server response the
//!   same way. This is the "Rust client against a TS server" direction at the
//!   byte level.
//! - `handler_cases_*` / `socket_requests_*`: TS server -> wire response; the
//!   Rust handler/server must reproduce the status, content type and body
//!   bytes. This is "TS client against a Rust server" at the byte level.
//! - `flow_*`: the recorded requests are the exact bytes a real TS client sent
//!   to a real TS server; replaying them through Rust's FileStore-backed
//!   handler and feeding the recorded responses back through the Rust client
//!   exercises both directions end to end.
//! - Loopback tests need `TcpListener::bind`; when the sandbox denies it they
//!   fail loudly instead of passing, with the command for an outside rerun.

use bytes::Bytes;
use http_body_util::Full;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use pilotdeck_gateway::user_dialog::{
    DialogAnswer, DialogError, DialogRuntime, FileGatewayUserDialogStore,
    GatewayUserDialogStoreHttpServer, HttpGatewayUserDialogStore,
    HttpGatewayUserDialogStoreOptions, HttpStoreHttpRequest, HttpStoreLeaseClaim,
    HttpStoreOwnerClaim, HttpStoreTransport, HttpStoreTransportError, HttpStoreTransportRequest,
    HttpStoreTransportResponse, Lease, LeaseClaim, LiveGatewayUserDialogStore, OwnerClaim,
    Result as UserDialogResult, StoreKey, StoreOptions, StoredAnswer, StoredDialog,
    UserDialogStore, create_gateway_user_dialog_store_http_handler,
    start_gateway_user_dialog_store_http_server,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

const ORACLE: &str = include_str!("fixtures/user-dialog-http-store/oracle.json");
const TOKEN: &str = "test-dialog-store-token";
const DIALOG_REQUEST: &str = concat!(
    r#"{"type":"user_dialog_request","requestId":"http-dialog-1","dialogKind":"form","#,
    r#""toolCallId":"tool-1","toolName":"request_user_form","prompt":"Configure the deployment.","#,
    r#""schema":{"type":"object","properties":{"region":{"type":"string"},"count":{"type":"number"}},"#,
    r#""required":["region"],"additionalProperties":false}}"#
);
const UNICODE_REQUEST: &str = concat!(
    r#"{"type":"user_dialog_request","requestId":"r-\"quoted\"\n","dialogKind":"confirm","#,
    r#""toolCallId":"tool-😀","toolName":"request_user_confirm","#,
    r#""prompt":"line1\nline2\t\"quoted\" \\ back","#,
    r#""meta":{"big":1e21,"small":1.5,"negativeZero":-0,"safe":9007199254740991,"#,
    r#""array":[1,"two",null,{"deep":true}]}}"#
);

type Outcome = std::result::Result<Option<JsValue>, DialogError>;

fn oracle() -> Value {
    serde_json::from_str(ORACLE).expect("fixture oracle.json is valid JSON")
}

fn js(text: &str) -> JsValue {
    parse_json(text).unwrap_or_else(|_| panic!("fixture JSON: {text}"))
}

fn json(value: &JsValue) -> Value {
    let text = stringify_json(value)
        .unwrap_or_else(|error| panic!("stringify: {error}"))
        .expect("defined value");
    serde_json::from_str(&text).expect("serialized JSON")
}

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "hex length");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn key() -> StoreKey {
    StoreKey {
        project_root: "/workspace/project".into(),
        pilot_home: "/workspace/home".into(),
        session_id: "sdk:http-dialog-store".into(),
    }
}

fn dialog() -> StoredDialog {
    StoredDialog {
        request: js(DIALOG_REQUEST),
        created_at: "2026-09-11T00:00:00.000Z".into(),
    }
}

fn unicode_key() -> StoreKey {
    StoreKey {
        project_root: "/项目/😀".into(),
        pilot_home: "/home/\u{e9}".into(),
        session_id: "s\t1".into(),
    }
}

fn unicode_dialog() -> StoredDialog {
    StoredDialog {
        request: js(UNICODE_REQUEST),
        created_at: "2026-09-11T00:00:00.000Z".into(),
    }
}

fn blank_key() -> StoreKey {
    StoreKey {
        project_root: "  ".into(),
        pilot_home: "".into(),
        session_id: "s".into(),
    }
}

fn dialogs_value(dialogs: &[StoredDialog]) -> JsValue {
    JsValue::Array(dialogs.iter().map(StoredDialog::to_value).collect())
}

fn key_value(key: &StoreKey) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "projectRoot".into(),
        JsValue::String(key.project_root.clone()),
    );
    object.insert("pilotHome".into(), JsValue::String(key.pilot_home.clone()));
    object.insert("sessionId".into(), JsValue::String(key.session_id.clone()));
    JsValue::Object(object)
}

fn object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (name, value) in fields {
        object.insert(name.into(), value);
    }
    JsValue::Object(object)
}

fn assert_outcome(case: &str, result: Outcome, expected: &Value) {
    match expected["kind"].as_str().unwrap() {
        "error" => {
            let error = match result {
                Err(error) => error,
                Ok(value) => panic!("{case}: expected error, got {value:?}"),
            };
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{case}: error message"
            );
        }
        "value" => {
            let value = result.unwrap_or_else(|error| panic!("{case}: unexpected error {error}"));
            if expected["defined"].as_bool().unwrap() {
                assert_eq!(
                    json(&value.expect("defined outcome")),
                    expected["value"].clone(),
                    "{case}: outcome value"
                );
            } else {
                assert!(value.is_none(), "{case}: expected undefined, got {value:?}");
            }
        }
        other => panic!("{case}: unknown outcome kind {other}"),
    }
}

// ---------------------------------------------------------------------------
// Injectable client transport
// ---------------------------------------------------------------------------

type TransportResult = std::result::Result<HttpStoreTransportResponse, HttpStoreTransportError>;

struct ScriptedTransport {
    response: Mutex<Option<TransportResult>>,
    pending: bool,
    requests: Mutex<Vec<HttpStoreTransportRequest>>,
}
impl ScriptedTransport {
    fn reply(status: u16, body: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            response: Mutex::new(Some(Ok(HttpStoreTransportResponse {
                status,
                body: body.to_vec(),
            }))),
            pending: false,
            requests: Mutex::new(Vec::new()),
        })
    }
    fn error(message: &str) -> Arc<Self> {
        Arc::new(Self {
            response: Mutex::new(Some(Err(HttpStoreTransportError::new(message)))),
            pending: false,
            requests: Mutex::new(Vec::new()),
        })
    }
    fn pending() -> Arc<Self> {
        Arc::new(Self {
            response: Mutex::new(None),
            pending: true,
            requests: Mutex::new(Vec::new()),
        })
    }
    fn requests(&self) -> Vec<HttpStoreTransportRequest> {
        self.requests.lock().unwrap().clone()
    }
}
impl HttpStoreTransport for ScriptedTransport {
    fn post<'a>(
        &'a self,
        request: HttpStoreTransportRequest,
    ) -> Pin<Box<dyn Future<Output = TransportResult> + Send + 'a>> {
        self.requests.lock().unwrap().push(request);
        if self.pending {
            return Box::pin(std::future::pending::<TransportResult>());
        }
        let response = self
            .response
            .lock()
            .unwrap()
            .take()
            .expect("exactly one scripted response");
        Box::pin(async move { response })
    }
}

fn make_client(
    url: &str,
    timeout_ms: Option<f64>,
    transport: Arc<ScriptedTransport>,
) -> HttpGatewayUserDialogStore {
    HttpGatewayUserDialogStore::with_transport(
        HttpGatewayUserDialogStoreOptions {
            url: url.into(),
            authorization_token: TOKEN.into(),
            timeout_ms,
        },
        transport as Arc<dyn HttpStoreTransport>,
    )
    .expect("valid client options")
}

fn assert_recorded_request(case: &str, expected: &Value, request: &HttpStoreTransportRequest) {
    assert_eq!(
        request.url,
        expected["url"].as_str().unwrap(),
        "{case}: request URL"
    );
    let headers = expected["headers"].as_array().unwrap();
    let mut authorization = None;
    let mut content_type = None;
    for pair in headers {
        match pair[0].as_str().unwrap() {
            "authorization" => authorization = Some(pair[1].as_str().unwrap()),
            "content-type" => content_type = Some(pair[1].as_str().unwrap()),
            other => panic!("{case}: unexpected header {other}"),
        }
    }
    assert_eq!(
        request.authorization,
        authorization.expect("authorization header"),
        "{case}: authorization"
    );
    assert_eq!(
        request.content_type,
        content_type.expect("content-type header"),
        "{case}: content-type"
    );
    assert_eq!(
        request.body,
        expected["body"].as_str().unwrap().as_bytes(),
        "{case}: body bytes"
    );
}

// ---------------------------------------------------------------------------
// Client -> TS server recordings
// ---------------------------------------------------------------------------

async fn run_client_request(name: &str, store: &HttpGatewayUserDialogStore) -> Outcome {
    match name {
        "put-form" => store.put(&key(), &dialog()).await.map(|_| None),
        "put-undefined-member" => {
            let mut dialog = dialog();
            if let JsValue::Object(request) = &mut dialog.request {
                request.insert("schema".into(), JsValue::Undefined);
            }
            store.put(&key(), &dialog).await.map(|_| None)
        }
        "put-unicode-and-numbers" => store
            .put(&unicode_key(), &unicode_dialog())
            .await
            .map(|_| None),
        "put-blank-key" => store.put(&blank_key(), &dialog()).await.map(|_| None),
        "list" => store
            .list(&key())
            .await
            .map(|dialogs| Some(dialogs_value(&dialogs))),
        "remove" => store
            .remove(&key(), &"http-dialog-1".into())
            .await
            .map(|_| None),
        "clear" => store.clear(&key()).await.map(|_| None),
        "listLive" => store
            .list_live(&key())
            .await
            .map(|dialogs| Some(dialogs_value(&dialogs))),
        "claimLive-minimal" => store
            .claim_live(&key(), &"http-dialog-1".into(), 5000.0, None)
            .await
            .map(|claim| Some(claim.to_value())),
        "claimLive-renew-lease" => store
            .claim_live(
                &key(),
                &"http-dialog-1".into(),
                5000.0,
                Some(&"lease-1".into()),
            )
            .await
            .map(|claim| Some(claim.to_value())),
        "claimLive-fractional-ttl" => store
            .claim_live(&key(), &"http-dialog-1".into(), 1.5, None)
            .await
            .map(|claim| Some(claim.to_value())),
        "releaseLive" => store
            .release_live(&key(), &"http-dialog-1".into(), &"lease-1".into())
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "submitLiveAnswer-answered" => store
            .submit_live_answer(
                &key(),
                &"http-dialog-1".into(),
                Some(&"lease-1".into()),
                &DialogAnswer::Answered(js(r#"{"region":"us-east-1","count":2}"#)),
            )
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "submitLiveAnswer-cancelled-reason" => store
            .submit_live_answer(
                &key(),
                &"http-dialog-1".into(),
                None,
                &DialogAnswer::Cancelled(Some("user closed".into())),
            )
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "submitLiveAnswer-cancelled-bare" => store
            .submit_live_answer(
                &key(),
                &"http-dialog-1".into(),
                None,
                &DialogAnswer::Cancelled(None),
            )
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "submitLiveAnswer-undefined-value" => {
            let value = {
                let mut object = JsObject::new();
                object.insert("keep".into(), JsValue::Number(1.0));
                object.insert("drop".into(), JsValue::Undefined);
                JsValue::Object(object)
            };
            store
                .submit_live_answer(
                    &key(),
                    &"http-dialog-1".into(),
                    None,
                    &DialogAnswer::Answered(value),
                )
                .await
                .map(|value| Some(JsValue::Bool(value)))
        }
        "takeLiveAnswer" => store
            .take_live_answer(&key(), &"http-dialog-1".into())
            .await
            .map(|answer| answer.map(|answer| answer.to_value())),
        "claimLiveOwner" => store
            .claim_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into(), 5000.0)
            .await
            .map(|claim| Some(claim.to_value())),
        "renewLiveOwner" => store
            .renew_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into(), 5000.0)
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "releaseLiveOwner" => store
            .release_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into())
            .await
            .map(|value| Some(JsValue::Bool(value))),
        name if name.starts_with("endpoint-") => store.clear(&key()).await.map(|_| None),
        other => panic!("unhandled client request case {other}"),
    }
}

#[tokio::test]
async fn client_requests_match_typescript_bytes_and_values() {
    let oracle = oracle();
    for case in oracle["clientRequests"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert_eq!(
            case["request"]["method"].as_str(),
            Some("POST"),
            "{name}: method"
        );
        let status = case["serverResponse"]["status"].as_u64().unwrap() as u16;
        let body = case["serverResponse"]["body"].as_str().unwrap();
        let transport = ScriptedTransport::reply(status, body.as_bytes());
        let endpoint = case["endpoint"].as_str().unwrap();
        let store = make_client(endpoint, None, transport.clone());
        let outcome = run_client_request(name, &store).await;
        let requests = transport.requests();
        assert_eq!(requests.len(), 1, "{name}: one request");
        assert_recorded_request(name, &case["request"], &requests[0]);
        assert_outcome(name, outcome, &case["outcome"]);
    }
}

// ---------------------------------------------------------------------------
// Injectable response -> TS client outcome
// ---------------------------------------------------------------------------

async fn run_canonical(operation: &str, store: &HttpGatewayUserDialogStore) -> Outcome {
    match operation {
        "put" => store.put(&key(), &dialog()).await.map(|_| None),
        "list" => store
            .list(&key())
            .await
            .map(|dialogs| Some(dialogs_value(&dialogs))),
        "remove" => store
            .remove(&key(), &"http-dialog-1".into())
            .await
            .map(|_| None),
        "clear" => store.clear(&key()).await.map(|_| None),
        "listLive" => store
            .list_live(&key())
            .await
            .map(|dialogs| Some(dialogs_value(&dialogs))),
        "claimLive" => store
            .claim_live(&key(), &"http-dialog-1".into(), 5000.0, None)
            .await
            .map(|claim| Some(claim.to_value())),
        "releaseLive" => store
            .release_live(&key(), &"http-dialog-1".into(), &"lease-1".into())
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "submitLiveAnswer" => store
            .submit_live_answer(
                &key(),
                &"http-dialog-1".into(),
                Some(&"lease-1".into()),
                &DialogAnswer::Answered(js(r#"{"a":1}"#)),
            )
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "takeLiveAnswer" => store
            .take_live_answer(&key(), &"http-dialog-1".into())
            .await
            .map(|answer| answer.map(|answer| answer.to_value())),
        "claimLiveOwner" => store
            .claim_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into(), 5000.0)
            .await
            .map(|claim| Some(claim.to_value())),
        "renewLiveOwner" => store
            .renew_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into(), 5000.0)
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "releaseLiveOwner" => store
            .release_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into())
            .await
            .map(|value| Some(JsValue::Bool(value))),
        other => panic!("unhandled client response operation {other}"),
    }
}

#[tokio::test(start_paused = true)]
async fn client_responses_match_typescript_validation() {
    let oracle = oracle();
    for case in oracle["clientResponses"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let injection = &case["injection"];
        let transport = match injection["kind"].as_str().unwrap() {
            "reply" => ScriptedTransport::reply(
                injection["status"].as_u64().unwrap() as u16,
                injection["body"].as_str().unwrap().as_bytes(),
            ),
            "throw" => ScriptedTransport::error(injection["message"].as_str().unwrap()),
            "abort" => ScriptedTransport::pending(),
            // A rejecting `json()` leaves the body undefined; malformed bytes
            // have the same observable effect on the Rust side.
            "jsonRejects" => {
                ScriptedTransport::reply(injection["status"].as_u64().unwrap() as u16, b"not json")
            }
            other => panic!("{name}: unknown injection {other}"),
        };
        let timeout = case["timeoutMs"].as_f64();
        let store = make_client("http://127.0.0.1:9/base", timeout, transport.clone());
        let outcome = run_canonical(case["operation"].as_str().unwrap(), &store).await;
        assert_eq!(transport.requests().len(), 1, "{name}: one request");
        assert_outcome(name, outcome, &case["outcome"]);
    }
}

// ---------------------------------------------------------------------------
// Handler recordings
// ---------------------------------------------------------------------------

struct RecordingStore {
    calls: Mutex<Vec<JsValue>>,
    failing: bool,
}
impl RecordingStore {
    fn new(failing: bool) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            failing,
        }
    }
    fn calls(&self) -> Vec<JsValue> {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, fields: Vec<(&str, JsValue)>) {
        self.calls.lock().unwrap().push(object(fields));
    }
}
impl UserDialogStore for RecordingStore {
    fn put(&self, key: &StoreKey, dialog: &StoredDialog) -> UserDialogResult<()> {
        // The TS failing double throws before recording the call.
        if self.failing {
            return Err(DialogError::Invalid("plain failure".into()));
        }
        self.record(vec![
            ("op", JsValue::from("put")),
            ("key", key_value(key)),
            ("dialog", dialog.to_value()),
        ]);
        Ok(())
    }
    fn remove(&self, key: &StoreKey, request_id: &JsString) -> UserDialogResult<()> {
        self.record(vec![
            ("op", JsValue::from("remove")),
            ("key", key_value(key)),
            ("requestId", JsValue::String(request_id.clone())),
        ]);
        Ok(())
    }
}
impl LiveGatewayUserDialogStore for RecordingStore {
    fn list(&self, key: &StoreKey) -> UserDialogResult<Vec<StoredDialog>> {
        if self.failing {
            return Err(DialogError::Invalid("disk exploded".into()));
        }
        self.record(vec![("op", JsValue::from("list")), ("key", key_value(key))]);
        Ok(vec![dialog()])
    }
    fn clear(&self, key: &StoreKey) -> UserDialogResult<()> {
        if self.failing {
            return Err(DialogError::Invalid("bad type".into()));
        }
        self.record(vec![
            ("op", JsValue::from("clear")),
            ("key", key_value(key)),
        ]);
        Ok(())
    }
    fn list_live(&self, key: &StoreKey) -> UserDialogResult<Vec<StoredDialog>> {
        self.record(vec![
            ("op", JsValue::from("listLive")),
            ("key", key_value(key)),
        ]);
        Ok(vec![dialog()])
    }
    fn claim_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        ttl_ms: f64,
        lease_id: Option<&JsString>,
    ) -> UserDialogResult<LeaseClaim> {
        let mut input = vec![
            ("requestId", JsValue::String(request_id.clone())),
            ("ttlMs", JsValue::Number(ttl_ms)),
        ];
        if let Some(lease_id) = lease_id {
            input.push(("leaseId", JsValue::String(lease_id.clone())));
        }
        self.record(vec![
            ("op", JsValue::from("claimLive")),
            ("key", key_value(key)),
            ("input", object(input)),
        ]);
        Ok(LeaseClaim::Claimed(Lease {
            lease_id: "lease-1".into(),
            expires_at: "2026-01-01T00:00:05.000Z".into(),
        }))
    }
    fn release_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: &JsString,
    ) -> UserDialogResult<bool> {
        self.record(vec![
            ("op", JsValue::from("releaseLive")),
            ("key", key_value(key)),
            (
                "input",
                object(vec![
                    ("requestId", JsValue::String(request_id.clone())),
                    ("leaseId", JsValue::String(lease_id.clone())),
                ]),
            ),
        ]);
        Ok(true)
    }
    fn submit_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: Option<&JsString>,
        result: &DialogAnswer,
    ) -> UserDialogResult<bool> {
        let mut input = vec![("requestId", JsValue::String(request_id.clone()))];
        if let Some(lease_id) = lease_id {
            input.push(("leaseId", JsValue::String(lease_id.clone())));
        }
        input.push(("result", result.to_value()));
        self.record(vec![
            ("op", JsValue::from("submitLiveAnswer")),
            ("key", key_value(key)),
            ("input", object(input)),
        ]);
        Ok(true)
    }
    fn take_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
    ) -> UserDialogResult<Option<StoredAnswer>> {
        self.record(vec![
            ("op", JsValue::from("takeLiveAnswer")),
            ("key", key_value(key)),
            ("requestId", JsValue::String(request_id.clone())),
        ]);
        Ok(Some(StoredAnswer {
            request_id: "http-dialog-1".into(),
            result: DialogAnswer::Cancelled(Some("why".into())),
            submitted_at: "2026-01-01T00:00:04.000Z".into(),
        }))
    }
    fn claim_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> UserDialogResult<OwnerClaim> {
        self.record(vec![
            ("op", JsValue::from("claimLiveOwner")),
            ("key", key_value(key)),
            (
                "input",
                object(vec![
                    ("requestId", JsValue::String(request_id.clone())),
                    ("ownerId", JsValue::String(owner_id.clone())),
                    ("ttlMs", JsValue::Number(ttl_ms)),
                ]),
            ),
        ]);
        Ok(OwnerClaim::Owned {
            owner_id: "gateway-a".into(),
            expires_at: "2026-01-01T00:00:05.000Z".into(),
        })
    }
    fn renew_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> UserDialogResult<bool> {
        self.record(vec![
            ("op", JsValue::from("renewLiveOwner")),
            ("key", key_value(key)),
            (
                "input",
                object(vec![
                    ("requestId", JsValue::String(request_id.clone())),
                    ("ownerId", JsValue::String(owner_id.clone())),
                    ("ttlMs", JsValue::Number(ttl_ms)),
                ]),
            ),
        ]);
        Ok(true)
    }
    fn release_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
    ) -> UserDialogResult<bool> {
        self.record(vec![
            ("op", JsValue::from("releaseLiveOwner")),
            ("key", key_value(key)),
            (
                "input",
                object(vec![
                    ("requestId", JsValue::String(request_id.clone())),
                    ("ownerId", JsValue::String(owner_id.clone())),
                ]),
            ),
        ]);
        Ok(true)
    }
}

fn recorded_request(request: &Value) -> HttpStoreHttpRequest {
    let headers = request["headers"]
        .as_array()
        .map(|pairs| {
            pairs
                .iter()
                .map(|pair| {
                    (
                        pair[0].as_str().unwrap().to_string(),
                        pair[1].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let body = if let Some(body) = request.get("bodyHex").and_then(Value::as_str) {
        hex(body)
    } else {
        request
            .get("body")
            .and_then(Value::as_str)
            .map(|body| body.as_bytes().to_vec())
            .unwrap_or_default()
    };
    HttpStoreHttpRequest {
        method: request["method"].as_str().unwrap().to_string(),
        url: request["url"].as_str().unwrap().to_string(),
        headers,
        body,
    }
}

#[test]
fn handler_cases_match_typescript_bytes() {
    let oracle = oracle();
    for case in oracle["handlerCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let failing = case["store"].as_str().unwrap() == "failing";
        let recording = Arc::new(RecordingStore::new(failing));
        let handler =
            create_gateway_user_dialog_store_http_handler(recording.clone(), TOKEN).unwrap();
        let response = handler.handle(recorded_request(&case["request"]));
        assert_eq!(
            response.status,
            case["response"]["status"].as_u64().unwrap() as u16,
            "{name}: status"
        );
        assert_eq!(
            response.content_type,
            case["response"]["contentType"].as_str().unwrap(),
            "{name}: content type"
        );
        assert_eq!(
            response.body,
            case["response"]["body"].as_str().unwrap().as_bytes(),
            "{name}: body"
        );
        let actual: Vec<Value> = recording.calls().iter().map(json).collect();
        assert_eq!(
            Value::Array(actual),
            case["calls"].clone(),
            "{name}: store call arguments"
        );
    }
}

// ---------------------------------------------------------------------------
// flow: real TS client <-> real TS server, replayed through Rust
// ---------------------------------------------------------------------------

fn flow_server_store(
    directory: std::path::PathBuf,
    uuid_index: Arc<std::sync::atomic::AtomicUsize>,
) -> Arc<FileGatewayUserDialogStore> {
    let mut options = StoreOptions::new(directory);
    options.runtime = DialogRuntime {
        now: Arc::new(|| 1_767_225_600_000),
        uuid: Arc::new(move || {
            let index = uuid_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ["lease-1", "lease-2"][index % 2].to_string()
        }),
    };
    Arc::new(FileGatewayUserDialogStore::new(options).unwrap())
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("pdrs-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn flow_request(request: &Value) -> HttpStoreHttpRequest {
    let headers = request["headers"]
        .as_array()
        .map(|pairs| {
            pairs
                .iter()
                .map(|pair| {
                    (
                        pair[0].as_str().unwrap().to_string(),
                        pair[1].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let body = request["body"]
        .as_str()
        .map(|body| body.as_bytes().to_vec())
        .unwrap_or_default();
    HttpStoreHttpRequest {
        method: request["method"].as_str().unwrap().to_string(),
        url: format!("http://127.0.0.1:1234{}", request["path"].as_str().unwrap()),
        headers,
        body,
    }
}

async fn run_flow_client(name: &str, store: &HttpGatewayUserDialogStore) -> Outcome {
    match name {
        "put" => store.put(&key(), &dialog()).await.map(|_| None),
        "list" | "list-after-clear" => store
            .list(&key())
            .await
            .map(|dialogs| Some(dialogs_value(&dialogs))),
        "listLive-empty" | "listLive" | "listLive-after-release" => store
            .list_live(&key())
            .await
            .map(|dialogs| Some(dialogs_value(&dialogs))),
        "claimLiveOwner" => store
            .claim_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into(), 5000.0)
            .await
            .map(|claim| Some(claim.to_value())),
        "renewLiveOwner" => store
            .renew_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into(), 5000.0)
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "claimLive" => store
            .claim_live(&key(), &"http-dialog-1".into(), 5000.0, None)
            .await
            .map(|claim| Some(claim.to_value())),
        "submitLiveAnswer-wrong-lease" | "submitLiveAnswer" => {
            let lease = if name.ends_with("wrong-lease") {
                "wrong-lease"
            } else {
                "lease-1"
            };
            store
                .submit_live_answer(
                    &key(),
                    &"http-dialog-1".into(),
                    Some(&lease.into()),
                    &DialogAnswer::Answered(js(r#"{"region":"us-east-1"}"#)),
                )
                .await
                .map(|value| Some(JsValue::Bool(value)))
        }
        "takeLiveAnswer" | "takeLiveAnswer-again" => store
            .take_live_answer(&key(), &"http-dialog-1".into())
            .await
            .map(|answer| answer.map(|answer| answer.to_value())),
        "releaseLive" => store
            .release_live(&key(), &"http-dialog-1".into(), &"lease-1".into())
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "releaseLiveOwner" => store
            .release_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into())
            .await
            .map(|value| Some(JsValue::Bool(value))),
        "clear" => store.clear(&key()).await.map(|_| None),
        other => panic!("unhandled flow step {other}"),
    }
}

#[tokio::test]
async fn flow_matches_typescript_on_both_sides() {
    let oracle = oracle();
    let steps = oracle["flow"].as_array().unwrap();

    // Server side: exact TS client request bytes -> Rust FileStore handler.
    let directory = temp_dir("user-dialog-http-store-flow");
    let uuid_index = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let store = flow_server_store(directory.clone(), uuid_index);
    let handler = create_gateway_user_dialog_store_http_handler(store, TOKEN).unwrap();
    for step in steps {
        let name = step["name"].as_str().unwrap();
        let response = handler.handle(flow_request(&step["request"]));
        assert_eq!(
            response.status,
            step["response"]["status"].as_u64().unwrap() as u16,
            "{name}: server status"
        );
        assert_eq!(
            String::from_utf8_lossy(&response.body),
            step["response"]["body"].as_str().unwrap(),
            "{name}: server body"
        );
    }
    let _ = std::fs::remove_dir_all(&directory);

    // Client side: exact TS server response bytes -> Rust client outcome.
    for step in steps {
        let name = step["name"].as_str().unwrap();
        let transport = ScriptedTransport::reply(
            step["response"]["status"].as_u64().unwrap() as u16,
            step["response"]["body"].as_str().unwrap().as_bytes(),
        );
        let client = make_client(
            "http://127.0.0.1:1234/v1/gateway-user-dialog-store/",
            None,
            transport.clone(),
        );
        let outcome = run_flow_client(name, &client).await;
        assert_eq!(transport.requests().len(), 1, "{name}: one request");
        let request = &transport.requests()[0];
        let expected = &step["request"];
        assert_eq!(
            request.url,
            format!(
                "http://127.0.0.1:1234{}",
                expected["path"].as_str().unwrap()
            ),
            "{name}: request path"
        );
        assert_eq!(
            request.authorization,
            format!("Bearer {TOKEN}"),
            "{name}: authorization"
        );
        assert_eq!(
            request.content_type, "application/json",
            "{name}: content-type"
        );
        assert_eq!(
            request.body,
            expected["body"].as_str().unwrap().as_bytes(),
            "{name}: request body"
        );
        assert_outcome(name, outcome, &step["outcome"]);
    }
}

// ---------------------------------------------------------------------------
// Loopback framing (needs TcpListener::bind; sandbox may deny it)
// ---------------------------------------------------------------------------

async fn loopback_server(
    store: Arc<dyn LiveGatewayUserDialogStore>,
) -> (GatewayUserDialogStoreHttpServer, String, u16) {
    let server =
        start_gateway_user_dialog_store_http_server(store, TOKEN, Some("127.0.0.1"), Some(0))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "loopback listen failed ({error}); this sandbox blocks TcpListener::bind. \
             Outside the sandbox rerun: cargo test -p pilotdeck-gateway --test \
             user_dialog_http_store -- --nocapture"
                )
            });
    let url = url::Url::parse(server.url()).expect("server URL");
    let port = url.port().expect("port");
    (server, url.to_string(), port)
}

/// #2152: the missing-Host cell replays over a raw socket — the `hyper`
/// client above pins its own `Host` header onto every request, and the cell
/// exists to send none. Returns the (status, content-type, body) the hyper
/// path would have compared, so both paths share one assertion.
async fn raw_hostless_exchange(port: u16, request: &[u8]) -> (u16, Option<String>, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // #2152 fix-r1 (review r1): the exchange is bounded like the sms lane's
    // `bounded` — the rejection is expected to close the connection, so a
    // regression that keeps it open must fail here, not hang the whole
    // test binary on a `read_to_end` that never returns.
    let raw = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("#2152 connect for the hostless replay");
        stream
            .write_all(request)
            .await
            .expect("#2152 write the hostless request");
        // The rejection carries `Connection: close`, so the server ends the
        // read.
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .await
            .expect("#2152 the hostless response must close the connection");
        raw
    })
    .await
    .expect("#2152 the hostless replay must finish within 5 s");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("#2152 the hostless response carries a header end");
    let status = head
        .split("\r\n")
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .expect("#2152 the hostless response carries a status code");
    let header_lines: Vec<&str> = head
        .split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    let content_type = header_lines.iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-type")
            .then(|| value.trim().to_string())
    });
    let chunked = header_lines.iter().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.trim().to_ascii_lowercase().contains("chunked")
        })
    });
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, content_type, body)
}

/// Strip HTTP/1.1 chunk framing (the hostless 400 answers with the empty
/// chunk `0\r\n\r\n`; a general strip keeps the assertion honest if that ever
/// changes).
fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some(line_end) = body.find("\r\n") {
        let Ok(size) = usize::from_str_radix(body[..line_end].trim(), 16) else {
            break;
        };
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(body.len());
        out.push_str(&body[..take]);
        body = &body[take..];
        if let Some(rest) = body.strip_prefix("\r\n") {
            body = rest;
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_requests_match_typescript_framing() {
    let oracle = oracle();
    let directory = temp_dir("user-dialog-http-store-socket");
    let store: Arc<dyn LiveGatewayUserDialogStore> =
        Arc::new(FileGatewayUserDialogStore::new(StoreOptions::new(directory.clone())).unwrap());
    let (server, _server_url, port) = loopback_server(store).await;
    let client: Client<HttpConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    for case in oracle["socketRequests"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let request = &case["request"];
        let path = request["path"].as_str().unwrap();
        let set_host = recorded_set_host(request, name);
        let mut builder = hyper::Request::builder()
            .method(request["method"].as_str().unwrap())
            .uri(format!("http://127.0.0.1:{port}{path}"));
        for pair in request["headers"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            builder = builder.header(pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
        }
        let body = if let Some(body) = request.get("bodyHex").and_then(Value::as_str) {
            hex(body)
        } else {
            request
                .get("body")
                .and_then(Value::as_str)
                .map(|body| body.as_bytes().to_vec())
                .unwrap_or_default()
        };
        let (status, content_type, wire_body) = if set_host {
            let request = builder.body(Full::new(Bytes::from(body))).expect("request");
            // #2152 fix-r2 (review r2): the hyper exchange is bounded like
            // the raw-socket branch in `raw_hostless_exchange` — the server's
            // handler phase has no deadline of its own, so a regression
            // that stalls the response must fail here, not hang the whole
            // test binary on a request that never answers.
            let (status, content_type, wire_body) =
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    let response = client
                        .request(request)
                        .await
                        .unwrap_or_else(|error| panic!("{name}: {error}"));
                    let status = response.status().as_u16();
                    let content_type = response
                        .headers()
                        .get(hyper::header::CONTENT_TYPE)
                        .map(|value| value.to_str().unwrap().to_string());
                    let wire_body = http_body_util::BodyExt::collect(response.into_body())
                        .await
                        .unwrap()
                        .to_bytes();
                    (status, content_type, wire_body.to_vec())
                })
                .await
                .unwrap_or_else(|_| {
                    panic!("{name}: #2152 the hyper replay must finish within 5 s")
                });
            (status, content_type, wire_body)
        } else {
            // The recorded method and path on a hand-built request line: the
            // hyper client would add its own `Host`, which is exactly the
            // header this cell must not carry.
            let mut raw = format!(
                "{} {} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                request["method"].as_str().unwrap(),
                path,
                body.len()
            )
            .into_bytes();
            raw.extend_from_slice(&body);
            let (status, content_type, body) = raw_hostless_exchange(port, &raw).await;
            (status, content_type, body.into_bytes())
        };
        assert_eq!(
            status,
            case["response"]["status"].as_u64().unwrap() as u16,
            "{name}: status"
        );
        assert_eq!(
            content_type.as_deref(),
            case["response"]["contentType"].as_str(),
            "{name}: content type"
        );
        assert_eq!(
            wire_body.as_slice(),
            case["response"]["body"].as_str().unwrap().as_bytes(),
            "{name}: body"
        );
    }
    server.close().await.expect("HTTP store server closes");
    let _ = std::fs::remove_dir_all(&directory);
}

/// #2152: the two parser-admission cells carry a knob the generator uses and
/// the replay must not silently degrade, the api-server lane's rule: if the
/// `unknown-method` cell ever records a served method, or `missing-host`
/// loses its `setHost: false`, the replay would compare a routed answer
/// against its own 400 and the gate could be removed unnoticed.
#[test]
fn socket_requests_pin_their_parser_admission_knobs() {
    let oracle = oracle();
    let cases = oracle["socketRequests"].as_array().unwrap();
    let unknown = cases
        .iter()
        .find(|case| case["name"].as_str() == Some("unknown-method"))
        .expect("#2152 the socketRequests carry the unknown-method cell");
    assert_eq!(
        unknown["request"]["method"].as_str(),
        Some("FROB"),
        "#2152 the unknown-method cell must record a token outside Node's \
         `http.METHODS`, or the replay compares a routed answer"
    );
    let missing = cases
        .iter()
        .find(|case| case["name"].as_str() == Some("missing-host"))
        .expect("#2152 the socketRequests carry the missing-host cell");
    assert_eq!(
        missing["request"]["setHost"].as_bool(),
        Some(false),
        "#2152 the missing-host cell must record `setHost: false`, or the \
         replay sends a Host header"
    );
}

/// #2152: absence is the normal request (a `Host` is sent); only a recorded
/// boolean may suppress it — the api-server lane's `setHost` rule. Anything
/// else is fixture drift that would otherwise replay a differently-framed
/// request.
fn recorded_set_host(request: &Value, name: &str) -> bool {
    match request.get("setHost") {
        None => true,
        Some(Value::Bool(set_host)) => *set_host,
        Some(other) => {
            panic!("#2152 request `setHost` must be a boolean, found {other:?} ({name})")
        }
    }
}

/// #2152 fix-r1 (review r1): the fail-closed `setHost` read has its own
/// unit pin, the api-server lane's `req_spec_refuses_a_non_boolean_set_host`
/// rule — the read is only embedded in the big replay otherwise.
#[test]
#[should_panic(expected = "#2152 request `setHost` must be a boolean, found String(\"no\"")]
fn socket_request_set_host_refuses_a_non_boolean() {
    // fix-r2 (review r2): `parse_json` returns `pilotdeck_protocol::JsValue`,
    // while `recorded_set_host` reads a `serde_json::Value` — round-trip
    // through the file's own `js`/`json` helpers, like every other reader
    // here, so the pin both compiles and keeps the serde Debug shape the
    // `should_panic` message asserts.
    let request = json(&js(r#"{"path": "/x", "setHost": "no"}"#));
    recorded_set_host(&request, "negative");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rust_client_and_server_round_trip() {
    let directory = temp_dir("user-dialog-http-store-roundtrip");
    let store: Arc<dyn LiveGatewayUserDialogStore> =
        Arc::new(FileGatewayUserDialogStore::new(StoreOptions::new(directory.clone())).unwrap());
    let (server, url, _port) = loopback_server(store).await;
    let client = HttpGatewayUserDialogStore::new(HttpGatewayUserDialogStoreOptions {
        url: url.clone(),
        authorization_token: TOKEN.into(),
        timeout_ms: Some(5_000.0),
    })
    .unwrap();
    client.put(&key(), &dialog()).await.unwrap();
    assert_eq!(client.list(&key()).await.unwrap(), vec![dialog()]);
    assert!(client.list_live(&key()).await.unwrap().is_empty());
    let owner = client
        .claim_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into(), 5000.0)
        .await
        .unwrap();
    assert!(matches!(owner, HttpStoreOwnerClaim::Owned { .. }));
    let claim = client
        .claim_live(&key(), &"http-dialog-1".into(), 5000.0, None)
        .await
        .unwrap();
    let lease = match claim {
        HttpStoreLeaseClaim::Claimed { lease_id, .. } => lease_id,
        other => panic!("expected a lease, got {other:?}"),
    };
    assert!(
        client
            .submit_live_answer(
                &key(),
                &"http-dialog-1".into(),
                Some(&lease),
                &DialogAnswer::Answered(js(r#"{"region":"us-east-1"}"#)),
            )
            .await
            .unwrap()
    );
    let answer = client
        .take_live_answer(&key(), &"http-dialog-1".into())
        .await
        .unwrap()
        .expect("answer");
    assert_eq!(
        answer.result,
        DialogAnswer::Answered(js(r#"{"region":"us-east-1"}"#))
    );
    assert!(
        client
            .release_live(&key(), &"http-dialog-1".into(), &lease)
            .await
            .unwrap()
    );
    assert!(
        client
            .release_live_owner(&key(), &"http-dialog-1".into(), &"gateway-a".into())
            .await
            .unwrap()
    );
    client.clear(&key()).await.unwrap();
    assert!(client.list(&key()).await.unwrap().is_empty());
    server.close().await.expect("HTTP store server closes");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn fixture_provenance_is_pinned() {
    let oracle = oracle();
    assert_eq!(
        oracle["sourceCommit"].as_str(),
        Some("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85")
    );
    assert!(oracle["baselineCount"].as_u64().unwrap() > 0);
    for entry in [
        "src/gateway/user-dialog/HttpGatewayUserDialogStore.ts",
        "src/gateway/user-dialog/FileGatewayUserDialogStore.ts",
    ] {
        let digest = oracle["sourceSha256"][entry].as_str().unwrap();
        assert_eq!(digest.len(), 64, "{entry}: sha256");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }
    for (name, count) in [
        ("clientRequests", 30),
        ("clientResponses", 69),
        ("clientBodyFailures", 6),
        ("handlerCases", 111),
        ("socketRequests", 20),
        ("flow", 16),
        ("constructors", 29),
        ("handlerConstruction", 7),
    ] {
        assert_eq!(oracle["counts"][name].as_u64(), Some(count), "{name}");
    }
}
