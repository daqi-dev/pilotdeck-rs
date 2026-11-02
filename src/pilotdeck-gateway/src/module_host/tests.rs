//! #1349: byte-level replay of the `module-host` golden fixture (every cell
//! one test), the raw-byte pins the replay normalization cannot express, the
//! same-source provider checks, and the no-child-process audit. The fixture
//! was recorded against the real TS stack (`startGatewayServer` + the real
//! `publicHostHttpTransport` + a scripted provider keyed on the request
//! principal's agentId); here the same requests go through the native hyper
//! listener with the module-host transport composed in front of the #1209
//! router, which serves the fallthrough 404s the TS server answers.
use super::*;
use crate::{http_proxy, native_http, test_baseline};
use bytes::Bytes;
use pilotdeck_protocol::{JsString, parse_json, stringify_json};
use serde_json::Value;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const ORACLE: &str = include_str!("../../tests/fixtures/module-host/oracle.json");
const DEADLINE: Duration = Duration::from_secs(10);
/// The recorder's quiet window: a response is complete after a quarter second
/// of silence while the connection stays open.
const QUIET: Duration = Duration::from_millis(250);

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1349 oracle JSON")
}

struct Cell {
    id: String,
    server: String,
    request: Vec<u8>,
    response: String,
    closed: bool,
    half_close: bool,
}

fn cell(id: &str) -> Cell {
    let value = oracle();
    let cells = value["cells"].as_array().expect("#1349 oracle cells array");
    for entry in cells {
        if entry["id"].as_str() == Some(id) {
            let mut request = entry["request"]
                .as_str()
                .expect("#1349 oracle cell request")
                .to_owned();
            // The budget cells carry the pad marker plus a size; the replay
            // writes the real 'a' padding the recorder wrote.
            if let Some(pad) = entry.get("pad").filter(|pad| !pad.is_null()) {
                let marker = pad["marker"].as_str().expect("#1349 oracle pad marker");
                let size = pad["size"].as_u64().expect("#1349 oracle pad size") as usize;
                assert!(
                    request.contains(marker),
                    "#1349 cell {id} pad marker missing from its request"
                );
                request = request.replace(marker, &"a".repeat(size));
            }
            return Cell {
                id: id.to_owned(),
                server: entry["server"]
                    .as_str()
                    .expect("#1349 oracle cell server")
                    .to_owned(),
                request: request.into_bytes(),
                response: entry["response"]
                    .as_str()
                    .expect("#1349 oracle cell response")
                    .to_owned(),
                closed: entry["closed"]
                    .as_bool()
                    .expect("#1349 oracle cell closed flag"),
                half_close: entry["halfClose"].as_bool().unwrap_or(false),
            };
        }
    }
    panic!("#1349 cell {id} is missing from the committed oracle");
}

// ---------------------------------------------------------------- provider

/// The scripted provider, the Rust mirror of the fixture's resolver: the
/// scenario is the request principal's agentId, and the operation table is
/// the one the describe-filter cells exercise.
struct Scripted;
impl PublicHostResolver for Scripted {
    fn resolve(&self, principal: &PublicHostPrincipal) -> Arc<dyn PublicHostProvider> {
        Arc::new(ScriptedProvider {
            agent: principal.agent_id.to_utf8().unwrap_or_default(),
        })
    }
}

struct ScriptedProvider {
    agent: String,
}

fn json(text: &str) -> JsValue {
    parse_json(text).expect("#1349 scenario JSON must parse")
}

fn host_response(status: u16, headers: &[(&str, &str)], body: HostBody) -> HostResponse {
    HostResponse {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body,
        raw_body: None,
    }
}

fn raw_response(status: u16, headers: &[(&str, &str)], raw_body: &str) -> HostResponse {
    HostResponse {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: HostBody::Value(JsValue::Undefined),
        raw_body: Some(raw_body.to_owned()),
    }
}

fn host_failure(status: Option<u16>, code: Option<&str>, message: Option<&str>) -> HostFailure {
    HostFailure {
        status,
        code: code.map(String::from),
        message: message.map(String::from),
    }
}

fn event_stream(events: Vec<HostEvent>) -> HostBody {
    let (sender, receiver) = mpsc::channel(events.len().max(1));
    for event in events {
        sender
            .try_send(Ok(event))
            .expect("#1349 event_stream channel must hold every event");
    }
    HostBody::Stream(receiver)
}

/// The scenario table, one arm per fixture scenario.
fn scenario(agent: &str) -> Result<HostResponse, HostFailure> {
    match agent {
        "plain" => Ok(host_response(
            200,
            &[("content-type", "text/plain")],
            HostBody::Value(json("\"hello\"")),
        )),
        "jsonstring" => Ok(host_response(
            200,
            &[("content-type", "application/json")],
            HostBody::Value(json("\"hello\"")),
        )),
        "emptystring_json" => Ok(host_response(
            200,
            &[("content-type", "application/json")],
            HostBody::Value(json("\"\"")),
        )),
        "bytes" => Ok(host_response(
            200,
            &[("content-type", "application/octet-stream")],
            HostBody::Bytes(Bytes::from_static(&[0, 1, 2, 0xFA])),
        )),
        "empty_ct_string" => Ok(host_response(
            200,
            &[("content-type", ""), ("etag", "W/\"x\"")],
            HostBody::Value(json("\"x\"")),
        )),
        "empty_ct_value" => Ok(host_response(
            200,
            &[("content-type", "")],
            HostBody::Value(json("{\"ok\":1}")),
        )),
        "empty_ct_bytes" => Ok(host_response(
            200,
            &[("content-type", "")],
            HostBody::Bytes(Bytes::from_static(&[0, 1, 2, 0xFA])),
        )),
        "rawbody" => Ok(raw_response(201, &[("content-type", "text/xml")], "<raw/>")),
        "rawbody_noct" => Ok(raw_response(202, &[], "raw-noct")),
        "nullbody" => Ok(host_response(200, &[], HostBody::Value(json("null")))),
        "undefbody" => Ok(host_response(200, &[], HostBody::Value(JsValue::Undefined))),
        "numberbody" => Ok(host_response(200, &[], HostBody::Value(json("5")))),
        "boolbody" => Ok(host_response(200, &[], HostBody::Value(json("false")))),
        "wireheaders" => Ok(host_response(
            200,
            &[
                ("content-type", "application/json"),
                ("etag", "W/\"a\""),
                ("retry-after", "3"),
                ("x-request-id", "req-1"),
                ("x-pilotdeck-model-id", "m-1"),
                ("x-pilotdeck-provider-id", "p-1"),
                ("x-drop-me", "dropped"),
                ("cache-control", "dropped"),
            ],
            HostBody::Value(json("{\"ok\":1}")),
        )),
        "bad_etag" => Ok(host_response(
            200,
            &[("etag", "bad\nvalue")],
            HostBody::Value(json("{}")),
        )),
        "bad_name" => Ok(host_response(
            200,
            &[("bad name", "x")],
            HostBody::Value(json("{}")),
        )),
        "fail_default" => Err(host_failure(None, None, Some("provider exploded"))),
        "fail_custom" => Err(host_failure(Some(400), Some("X_CUSTOM"), Some("nope"))),
        "fail_nomessage" => Err(host_failure(Some(418), None, None)),
        "cancel_499" => Ok(host_response(
            499,
            &[],
            HostBody::Value(json("{\"code\":\"PUBLIC_HOST_CANCELLED\"}")),
        )),
        "bad_status" => Ok(host_response(600, &[], HostBody::Value(json("{}")))),
        "ndjson_objects" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            event_stream(vec![
                HostEvent::Value(json("{\"a\":1}")),
                HostEvent::Value(json("{\"b\":\"two\"}")),
            ]),
        )),
        "ndjson_bytes" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            event_stream(vec![
                HostEvent::Bytes(Bytes::from_static(&[120, 121])),
                HostEvent::Text("str-text".to_owned()),
            ]),
        )),
        "sse_strings" => Ok(host_response(
            200,
            &[("content-type", "text/event-stream")],
            event_stream(vec![
                HostEvent::Text("data: a\n\n".to_owned()),
                HostEvent::Text("data: b\n\n".to_owned()),
            ]),
        )),
        "ndjson_array_event" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            event_stream(vec![
                HostEvent::Value(json("{\"a\":1}")),
                HostEvent::Value(json("[1,2]")),
            ]),
        )),
        "sse_object_event" => Ok(host_response(
            200,
            &[("content-type", "text/event-stream")],
            event_stream(vec![HostEvent::Value(json("{\"a\":1}"))]),
        )),
        "stream_plain_ct" => Ok(host_response(
            200,
            &[("content-type", "application/json")],
            event_stream(vec![HostEvent::Value(json("{\"a\":1}"))]),
        )),
        "stream_plain_ct_headers" => Ok(host_response(
            200,
            &[
                ("content-type", "application/json"),
                ("etag", "W/\"x\""),
                ("x-request-id", "r1"),
            ],
            event_stream(vec![HostEvent::Value(json("{\"a\":1}"))]),
        )),
        "stream_missing" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Value(json("null")),
        )),
        "stream_missing_headers" => Ok(host_response(
            200,
            &[
                ("content-type", "application/x-ndjson"),
                ("etag", "W/\"x\""),
                ("x-request-id", "r1"),
                ("retry-after", "3"),
            ],
            HostBody::Value(json("null")),
        )),
        "stream_noct_headers" => Ok(host_response(
            200,
            &[("etag", "W/\"x\""), ("x-request-id", "r1")],
            event_stream(vec![HostEvent::Value(json("{\"a\":1}"))]),
        )),
        "ndjson_empty_string" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Value(json("\"\"")),
        )),
        "ndjson_string_body" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Value(json("\"abc\"")),
        )),
        "ndjson_string_emoji" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Value(json("\"a😀b\"")),
        )),
        "ndjson_string_surrogate" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Value(json("\"\\ud800a\\ud83d\\ude00\"")),
        )),
        "ndjson_array_body" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Value(json("[\"a\",{\"b\":2}]")),
        )),
        "ndjson_object_body" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Value(json("{\"x\":1}")),
        )),
        "ndjson_bytes_body" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Bytes(Bytes::from_static(&[65, 66])),
        )),
        "ndjson_empty_bytes_body" => Ok(host_response(
            200,
            &[("content-type", "application/x-ndjson")],
            HostBody::Bytes(Bytes::new()),
        )),
        "ndjson_rawbody" => Ok(raw_response(
            200,
            &[("content-type", "application/x-ndjson")],
            "ignored",
        )),
        _ => Ok(host_response(
            200,
            &[],
            HostBody::Value(json("{\"scenario\":\"ok\"}")),
        )),
    }
}

impl PublicHostProvider for ScriptedProvider {
    fn operations(&self) -> Vec<String> {
        match self.agent.as_str() {
            "empty" => Vec::new(),
            "default" => [
                "list_tools",
                "custom_op",
                "list_general_skills",
                "model_prepare",
                "not_wire",
            ]
            .iter()
            .map(|operation| (*operation).to_owned())
            .collect(),
            _ => vec!["list_tools".to_owned()],
        }
    }

    fn call(
        &self,
        _operation: &str,
        _input: &JsValue,
        _principal: &PublicHostPrincipal,
    ) -> Pin<Box<dyn Future<Output = Result<HostResponse, HostFailure>> + Send + '_>> {
        Box::pin(std::future::ready(scenario(&self.agent)))
    }
}

// ----------------------------------------------------------------- harness

/// The module-host routing inputs of one fixture server: `main` carries the
/// scripted resolver, `bare` has none (the unconfigured 501 cells).
fn module_config_for(server: &str) -> Config {
    Config {
        token: "fixture-token".into(),
        capabilities: match server {
            "main" => Some(Arc::new(Scripted)),
            _ => None,
        },
    }
}

/// Bind one native listener and write the raw request bytes to it, then
/// capture the raw response bytes like the TS recorder did (quiet window,
/// EOF, or socket error ends the capture). The module-host transport is
/// composed in front of the #1209 router, which answers the fallthrough
/// 404s.
async fn exchange(server: &str, request: &[u8], id: &str, half_close: bool) -> (Vec<u8>, bool) {
    let module_config = module_config_for(server);
    let native_config = native_http::Config {
        token: "fixture-token".into(),
        loopback: true,
        static_root: None,
    };
    serve_and_capture(module_config, native_config, request, id, half_close).await
}

async fn serve_and_capture(
    module_config: Config,
    native_config: native_http::Config,
    request: &[u8],
    id: &str,
    half_close: bool,
) -> (Vec<u8>, bool) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#1349 bind listener");
    let bound = listener.local_addr().expect("#1349 listener local address");
    let server_task = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("#1349 accept connection");
            let module_config = module_config.clone();
            let native_config = native_config.clone();
            tokio::spawn(async move {
                // The production path always carries a stop signal; a sender
                // that is dropped on the spot is the no-stop case.
                let (_, stop) = tokio::sync::watch::channel(false);
                let served = http_proxy::serve_connection_native_with_shutdown(
                    socket,
                    move |mut request, _lease| {
                        let module_config = module_config.clone();
                        let native_config = native_config.clone();
                        async move {
                            match serve(&mut request, &module_config).await {
                                Some(answer) => answer,
                                None => native_http::serve(&request, &native_config).await,
                            }
                        }
                    },
                    stop,
                )
                .await;
                if let Err(error) = served {
                    // A handler Err (the truncated-body cell) is the
                    // deliberate zero-byte close.
                    eprintln!("#1349 module-host connection error: {error}");
                }
            });
        }
    });
    let mut stream = timeout(DEADLINE, TcpStream::connect(bound))
        .await
        .expect("#1349 connect deadline")
        .expect("#1349 connect to module-host listener");
    stream
        .write_all(request)
        .await
        .expect("#1349 write request bytes");
    if half_close {
        stream.shutdown().await.expect("#1349 half-close");
    }
    let captured = capture(&mut stream, id).await;
    server_task.abort();
    captured
}

async fn capture(stream: &mut TcpStream, id: &str) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 8192];
    let start = Instant::now();
    let mut closed = false;
    while start.elapsed() < DEADLINE {
        let wait = if out.is_empty() {
            DEADLINE - start.elapsed()
        } else {
            QUIET
        };
        match timeout(wait, stream.read(&mut buffer)).await {
            Err(_elapsed) if !out.is_empty() => break,
            Err(_elapsed) => continue,
            Ok(Ok(0)) => {
                closed = true;
                break;
            }
            Ok(Ok(read)) => out.extend_from_slice(&buffer[..read]),
            // The TS recorder ignored socket errors and kept the bytes that
            // had arrived, so a reset while the response drains is a close.
            Ok(Err(_reset)) => {
                closed = true;
                break;
            }
        }
    }
    assert!(
        !out.is_empty() || closed,
        "#1349 cell {id} produced no bytes and stayed open"
    );
    (out, closed)
}

/// One cell through the fixture replay: capture the raw response and compare
/// the normalized bytes and the closed flag against the golden oracle. The
/// oracle stores its latin1-mapped text, so both sides are compared as the
/// wire bytes they map to 1:1.
async fn replay(id: &str) {
    let cell = cell(id);
    let (raw, closed) = exchange(&cell.server, &cell.request, &cell.id, cell.half_close).await;
    assert_eq!(
        normalize(&raw),
        normalize(&latin1_bytes(&cell.response)),
        "#1349 cell {} normalized response",
        cell.id
    );
    assert_eq!(
        closed, cell.closed,
        "#1349 cell {} connection closed flag",
        cell.id
    );
}

macro_rules! replay_tests {
    ($($function:ident => $id:literal),* $(,)?) => {
        /// Every cell id the macro instantiates; a test asserts this plus the
        /// pinned ids is exactly the oracle's cell set, so neither list can
        /// drift alone.
        const REPLAY_CELL_IDS: &[&str] = &[$($id),*];
        $(
            #[tokio::test]
            async fn $function() {
                replay($id).await;
            }
        )*
    };
}

replay_tests! {
    auth_missing => "auth-missing",
    auth_wrong => "auth-wrong",
    auth_lower => "auth-lower",
    auth_dblspace => "auth-dblspace",
    auth_get_wrong => "auth-get-wrong",
    method_get => "method-get",
    method_put => "method-put",
    method_head => "method-head",
    method_get_body => "method-get-body",
    route_trailing => "route-trailing",
    route_calls => "route-calls",
    route_dot => "route-dot",
    route_pct => "route-pct",
    route_upper => "route-upper",
    route_dblslash => "route-dblslash",
    describe_query => "describe-query",
    unconfigured_describe => "unconfigured-describe",
    unconfigured_call => "unconfigured-call",
    body_bad_json => "body-bad-json",
    body_empty => "body-empty",
    body_number => "body-number",
    body_array => "body-array",
    body_null => "body-null",
    body_string => "body-string",
    principal_missing => "principal-missing",
    principal_empty => "principal-empty",
    principal_empty_string => "principal-empty-string",
    principal_whitespace => "principal-whitespace",
    principal_unicode_ws => "principal-unicode-ws",
    principal_nbsp => "principal-nbsp",
    principal_number => "principal-number",
    principal_three => "principal-three",
    describe_default => "describe-default",
    describe_empty => "describe-empty",
    describe_ok => "describe-ok",
    call_unknown => "call-unknown",
    call_missing_op => "call-missing-op",
    call_number_op => "call-number-op",
    call_not_advertised => "call-not-advertised",
    call_on_empty => "call-on-empty",
    input_missing => "input-missing",
    input_null => "input-null",
    input_string => "input-string",
    input_number => "input-number",
    input_array => "input-array",
    input_false => "input-false",
    scope_tenantid => "scope-tenantid",
    scope_snake => "scope-snake",
    scope_nested => "scope-nested",
    ok => "ok",
    plain => "plain",
    jsonstring => "jsonstring",
    emptystring_json => "emptystring-json",
    bytes => "bytes",
    empty_ct_string => "empty-ct-string",
    empty_ct_value => "empty-ct-value",
    empty_ct_bytes => "empty-ct-bytes",
    rawbody => "rawbody",
    rawbody_noct => "rawbody-noct",
    nullbody => "nullbody",
    undefbody => "undefbody",
    numberbody => "numberbody",
    boolbody => "boolbody",
    wireheaders => "wireheaders",
    bad_etag => "bad-etag",
    bad_header_name => "bad-header-name",
    fail_default => "fail-default",
    fail_custom => "fail-custom",
    fail_nomessage => "fail-nomessage",
    cancel_499 => "cancel-499",
    bad_status => "bad-status",
    ndjson_objects => "ndjson-objects",
    ndjson_bytes => "ndjson-bytes",
    sse_strings => "sse-strings",
    ndjson_array_event => "ndjson-array-event",
    sse_object_event => "sse-object-event",
    stream_plain_ct => "stream-plain-ct",
    stream_plain_ct_headers => "stream-plain-ct-headers",
    stream_missing => "stream-missing",
    stream_missing_headers => "stream-missing-headers",
    stream_noct_headers => "stream-noct-headers",
    ndjson_empty_string => "ndjson-empty-string",
    ndjson_string_body => "ndjson-string-body",
    ndjson_string_emoji => "ndjson-string-emoji",
    ndjson_string_surrogate => "ndjson-string-surrogate",
    ndjson_array_body => "ndjson-array-body",
    ndjson_object_body => "ndjson-object-body",
    ndjson_bytes_body => "ndjson-bytes-body",
    ndjson_empty_bytes_body => "ndjson-empty-bytes-body",
    ndjson_rawbody => "ndjson-rawbody",
    budget_exact => "budget-exact",
    budget_over => "budget-over",
    conn_close => "conn-close",
    keep_alive_reuse => "keep-alive-reuse",
}

/// The cells pinned raw instead of replayed: their bytes carry a registered
/// transport diff no normalization may swallow.
const PINNED_CELL_IDS: &[&str] = &["http10", "premature-close"];

/// #1349: the replay list plus the pinned list is the oracle's cell set; a
/// cell added to the oracle without a test (or vice versa) fails here.
#[test]
fn replay_cells_cover_exactly_the_oracle() {
    let recorded = oracle();
    let mut oracle_ids: Vec<&str> = recorded["cells"]
        .as_array()
        .expect("#1349 oracle cells array")
        .iter()
        .map(|cell| cell["id"].as_str().expect("#1349 oracle cell id"))
        .collect();
    oracle_ids.sort_unstable();
    let mut covered: Vec<&str> = REPLAY_CELL_IDS.to_vec();
    covered.extend_from_slice(PINNED_CELL_IDS);
    covered.sort_unstable();
    assert_eq!(
        covered, oracle_ids,
        "#1349 replay cell ids and oracle cell ids are the same set"
    );
}

/// #1349: an HTTP/1.0 request is answered with Node's own HTTP/1.1 status
/// line (oracle cell `http10`), while hyper writes its `HTTP/1.0` line — the
/// same registered diff #1209 pinned for this listener (ledger
/// A8.4-NATIVE-HTTP-001); everything else — the `Connection: close`, the
/// absent framing, the close-delimited body, the closed socket — replays.
#[tokio::test]
async fn http10_close_delimited_response() {
    let cell = cell("http10");
    let (bytes, closed) = exchange(&cell.server, &cell.request, &cell.id, cell.half_close).await;
    let raw = latin1_string(&bytes);
    assert!(
        raw.starts_with("HTTP/1.0 200 OK\r\n"),
        "#1349 pin http10: hyper answers with a 1.0 status line (Node's 1.1 line is the registered diff): {raw:?}"
    );
    assert!(
        raw.contains("\r\nConnection: close\r\n")
            && !raw.to_ascii_lowercase().contains("keep-alive"),
        "#1349 pin http10: the 1.0 response is close with no keep-alive like Node: {raw:?}"
    );
    assert!(
        raw.ends_with(
            "{\"operations\":[\"list_tools\",\"list_general_skills\",\"model_prepare\"]}"
        ),
        "#1349 pin http10: the 1.0 body is close-delimited, not chunked: {raw:?}"
    );
    assert!(
        closed,
        "#1349 pin http10: the 1.0 response closes the connection like Node"
    );
}

/// #1349: a request that closes before its declared body length is complete
/// draws Node's own parser-level `400 Connection: close` (oracle cell
/// `premature-close`), bytes the hyper typed response API cannot produce;
/// the transport itself writes nothing in either stack, and hyper's handler
/// error is the zero-byte close — the residual registered in
/// A8.4-MODULE-HOST-001.
#[tokio::test]
async fn premature_close_is_a_zero_byte_close() {
    let cell = cell("premature-close");
    let (raw, closed) = exchange(&cell.server, &cell.request, &cell.id, cell.half_close).await;
    assert!(
        raw.is_empty(),
        "#1349 pin premature-close: the truncated body closes with zero bytes (Node answers its own parser-level 400 Connection: close — the registered A8.4-MODULE-HOST-001 residual): {raw:?}"
    );
    assert!(
        closed,
        "#1349 pin premature-close: the truncated body closes the connection"
    );
}

// -------------------------------------------------------------- normalize

/// The bytes both sides are compared in, the #1209 rules: the Date value is
/// masked, header names are case-folded (hyper renders Title-Case, TS's
/// `content-type` is literal lowercase — the registered transport diff), the
/// empty-body `content-length: 0` line is dropped, and chunk sizes are
/// case-folded. Status lines are compared verbatim — both stacks write
/// canonical `HTTP/1.1 200 OK` casing, and the one code without a canonical
/// reason (499) is answered with Node's `unknown` through hyper's
/// `ReasonPhrase` extension — and chunk bodies are never folded. Everything
/// works on the raw wire bytes: a binary body (the `bytes` cell's `0xFA`)
/// keeps its 1:1 offsets, where the latin1 text the oracle stores would
/// shift them.
fn normalize(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    'responses: while !rest.is_empty() {
        let Some(head_end) = find(rest, b"\r\n\r\n") else {
            out.extend_from_slice(rest);
            break;
        };
        let head = &rest[..head_end];
        let mut chunked = false;
        let mut length = None;
        for line in split(head, b"\r\n").skip(1) {
            let Some((name, value)) = split_once(line, b":") else {
                continue;
            };
            let name = trim_ascii(name).to_ascii_lowercase();
            if name == b"transfer-encoding"
                && find(&value.to_ascii_lowercase(), b"chunked").is_some()
            {
                chunked = true;
            } else if name == b"content-length" {
                length = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.trim().parse::<usize>().ok());
            }
        }
        for (index, line) in split(head, b"\r\n").enumerate() {
            if index == 0 {
                out.extend_from_slice(line);
                out.extend_from_slice(b"\r\n");
                continue;
            }
            let Some((name, value)) = split_once(line, b":") else {
                out.extend_from_slice(line);
                out.extend_from_slice(b"\r\n");
                continue;
            };
            if name.eq_ignore_ascii_case(b"date") {
                out.extend_from_slice(b"date: <masked>\r\n");
            } else if name.eq_ignore_ascii_case(b"content-length") && trim_ascii(value) == b"0" {
                // hyper's implicit empty-body length.
            } else {
                out.extend_from_slice(&name.to_ascii_lowercase());
                out.extend_from_slice(b":");
                out.extend_from_slice(value);
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(b"\r\n");
        rest = &rest[head_end + 4..];
        if chunked {
            loop {
                let Some(line_end) = find(rest, b"\r\n") else {
                    out.extend_from_slice(rest);
                    break 'responses;
                };
                let field = split(&rest[..line_end], b";").next().unwrap_or(&[]);
                let size_text = String::from_utf8_lossy(field).into_owned();
                let size = usize::from_str_radix(size_text.trim(), 16)
                    .unwrap_or_else(|_| panic!("#1349 chunk size {size_text:?} is not hex"));
                out.extend_from_slice(size_text.to_ascii_lowercase().as_bytes());
                out.extend_from_slice(b"\r\n");
                rest = &rest[line_end + 2..];
                let chunk_end = size
                    .checked_add(2)
                    .expect("#1349 chunk size plus CRLF overflows usize");
                if rest.len() < chunk_end {
                    out.extend_from_slice(rest);
                    break 'responses;
                }
                out.extend_from_slice(&rest[..size]);
                out.extend_from_slice(b"\r\n");
                rest = &rest[chunk_end..];
                if size == 0 {
                    continue 'responses;
                }
            }
        }
        match length {
            Some(size) if rest.len() >= size => {
                out.extend_from_slice(&rest[..size]);
                rest = &rest[size..];
                continue 'responses;
            }
            _ => {
                out.extend_from_slice(rest);
                break;
            }
        }
    }
    out
}

/// Find the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Split on every `separator`, the `str::split` shape (a trailing separator
/// yields a final empty slice; an empty haystack yields nothing).
fn split<'a>(haystack: &'a [u8], separator: &[u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = haystack;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match find(rest, separator) {
            Some(at) => {
                let (line, tail) = rest.split_at(at);
                rest = &tail[separator.len()..];
                Some(line)
            }
            None => {
                let line = rest;
                rest = &[];
                Some(line)
            }
        }
    })
}

fn split_once<'a>(haystack: &'a [u8], separator: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    find(haystack, separator).map(|at| {
        let (before, after) = haystack.split_at(at);
        (before, &after[separator.len()..])
    })
}

/// Trim ASCII whitespace from both ends.
fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    &bytes[start..end]
}

/// The wire bytes the oracle's latin1-mapped response text names: every char
/// is one byte 0–255, so the map is 1:1.
fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars().map(|char| char as u8).collect()
}

fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

// ------------------------------------------------------------- same source

/// The native ports fixture: a temp home with a real user skill and a real
/// `pilotdeck.yaml`, the same store the WS modelCatalog RPC reads.
fn ports_fixture() -> (PathBuf, Arc<ModuleHostPorts>) {
    let parent = std::env::temp_dir().join(format!("pdrs-1349-{}", uuid::Uuid::new_v4()));
    let home = parent.join("home");
    std::fs::create_dir_all(home.join("skills").join("my-skill"))
        .expect("#1349 create the fixture home");
    std::fs::write(
        home.join("skills").join("my-skill").join("SKILL.md"),
        "---\nname: My Skill\ndescription: A same-source probe.\n---\nBody.\n",
    )
    .expect("#1349 write the fixture skill");
    std::fs::write(
        home.join("pilotdeck.yaml"),
        "schemaVersion: 1\nagent: {model: alpha/main}\nmodel:\n  providers:\n    alpha:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        main: {displayName: Main Model}\n        aux: {}\n    beta:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        solo: {}\n",
    )
    .expect("#1349 write the fixture config");
    let manager = Arc::new(
        crate::skills::SkillManager::new(crate::skills::SkillManagerOptions::new(
            home.to_string_lossy().into_owned(),
        ))
        .expect("#1349 construct the real SkillManager"),
    );
    let ports = Arc::new(ModuleHostPorts {
        skills: Some(SkillsPort {
            manager,
            project_key: None,
        }),
        catalog: Some(CatalogPort {
            metadata: crate::metadata::Metadata {
                home: home.clone(),
                cwd: parent.clone(),
                files: crate::metadata::NativeFiles,
            },
            env: crate::config::Env::new(),
        }),
    });
    (parent, ports)
}

fn principal_fixture() -> PublicHostPrincipal {
    PublicHostPrincipal {
        pilot_deck_user_id: JsString::from("u1"),
        tenant_id: JsString::from("t1"),
        actor_user_id: JsString::from("a1"),
        agent_id: JsString::from("native"),
    }
}

fn body_text(response: &HostResponse) -> String {
    match &response.body {
        HostBody::Value(value) => stringify_json(value)
            .expect("#1349 stringify the provider body")
            .unwrap_or_default(),
        _ => panic!("#1349 the same-source provider bodies are values"),
    }
}

/// #1349: describe is derived from the host's actual capability list, one
/// entry per installed port, and an uninstalled port removes its operations
/// (fail-closed — never advertise what cannot be called).
#[test]
fn operations_derive_from_installed_ports() {
    let none = ModuleHostPorts {
        skills: None,
        catalog: None,
    };
    assert_eq!(
        none.operations(),
        Vec::<String>::new(),
        "#1349 no installed port advertises nothing"
    );
    let skills_only = ModuleHostPorts {
        skills: Some(SkillsPort {
            manager: Arc::new(
                crate::skills::SkillManager::new(crate::skills::SkillManagerOptions::new("/tmp"))
                    .expect("#1349 construct the skills-only manager"),
            ),
            project_key: None,
        }),
        catalog: None,
    };
    assert_eq!(
        skills_only.operations(),
        vec![
            "list_general_skills".to_owned(),
            "import_general_skill".to_owned()
        ],
        "#1349 the skills port advertises exactly its two operations"
    );
    let (parent, both) = ports_fixture();
    assert_eq!(
        both.operations(),
        vec![
            "list_general_skills".to_owned(),
            "import_general_skill".to_owned(),
            "list_model_catalog".to_owned(),
        ],
        "#1349 both ports advertise their operations in order"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

/// #1349: the resolver adapter hands every principal the same installed-port
/// provider — the ports behind one `Arc`, the adapter the wiring lane (#1305)
/// passes as `Config::capabilities`. Exercised outside the Linux-only audit
/// so the adapter is constructed and called on every platform.
#[tokio::test]
async fn resolver_hands_back_the_installed_ports() {
    let (parent, ports) = ports_fixture();
    let resolver = ModuleHostResolver(Arc::clone(&ports));
    let principal = principal_fixture();
    let resolved = resolver.resolve(&principal);
    assert_eq!(
        resolved.operations(),
        ports.operations(),
        "#1349 the resolver hands back the installed ports' operations"
    );
    let response = resolved
        .call("list_model_catalog", &json("{}"), &principal)
        .await
        .expect("#1349 the resolved provider calls the catalog port");
    assert_eq!(
        response.status, 200,
        "#1349 the resolved provider serves the catalog"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

/// #1349: the skills port lists through the real SkillManager over the real
/// home, and the public result is the TS provider's shape.
#[tokio::test]
async fn skills_port_lists_over_a_real_manager() {
    let (parent, ports) = ports_fixture();
    let response = ports
        .call(
            "list_general_skills",
            &json("{\"scope\":\"all\"}"),
            &principal_fixture(),
        )
        .await
        .expect("#1349 the skills port call resolves");
    assert_eq!(
        response.status, 200,
        "#1349 the skills port lists over the real manager"
    );
    let text = body_text(&response);
    let body = parse_json(&text).expect("#1349 the skills list body is JSON");
    let mut keys: Vec<String> = body
        .as_object()
        .expect("#1349 the skills list body is an object")
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().unwrap_or_default())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "builtin".to_owned(),
            "data".to_owned(),
            "next_cursor".to_owned(),
            "project".to_owned(),
            "projectPath".to_owned(),
            "user".to_owned(),
        ],
        "#1349 the public list result carries exactly the TS provider's fields"
    );
    assert!(
        text.contains("my-skill"),
        "#1349 the public list result serves the real user skill"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

/// #1349: the catalog port reads the same config store the WS modelCatalog
/// RPC reads (`config::load`), and the public result is the TS provider's
/// derived shape — every provider/model pair, insertion order, the
/// displayName fallback, always enabled, and the selected default.
#[tokio::test]
async fn catalog_port_reads_the_real_config_store() {
    let (parent, ports) = ports_fixture();
    let response = ports
        .call("list_model_catalog", &json("{}"), &principal_fixture())
        .await
        .expect("#1349 the catalog port call resolves");
    assert_eq!(
        response.status, 200,
        "#1349 the catalog port loads the real config"
    );
    let text = body_text(&response);
    assert_eq!(
        text,
        "{\"data\":[{\"id\":\"alpha/main\",\"name\":\"Main Model\",\"model\":\"main\",\"provider\":\"alpha\",\"enabled\":true,\"is_default\":true},{\"id\":\"alpha/aux\",\"name\":\"aux\",\"model\":\"aux\",\"provider\":\"alpha\",\"enabled\":true,\"is_default\":false},{\"id\":\"beta/solo\",\"name\":\"solo\",\"model\":\"solo\",\"provider\":\"beta\",\"enabled\":true,\"is_default\":false}]}",
        "#1349 the public catalog is the TS provider's derived shape over the real config"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

/// #1349: an empty configured bearer token must not admit a bare
/// `Authorization: Bearer ` (or any whitespace-only token): the TS comparison
/// is `authorization !== \`Bearer ${token}\`` against a Node header value,
/// and both stacks strip the surrounding OWS before the comparison, so the
/// formatted `Bearer ` can never match a received header (measured on
/// hyper: every spacing is the 401).
#[tokio::test]
async fn empty_token_admits_no_bare_bearer() {
    let module_config = Config {
        token: String::new(),
        capabilities: None,
    };
    let native_config = native_http::Config {
        token: "fixture-token".into(),
        loopback: true,
        static_root: None,
    };
    for authorization in ["Bearer ", "Bearer", "Bearer  ", " Bearer "] {
        let request = format!(
            "POST /api/module-host/describe HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {authorization}\r\nContent-Length: 2\r\n\r\n{{}}"
        );
        let (raw, _) = serve_and_capture(
            module_config.clone(),
            native_config.clone(),
            request.as_bytes(),
            "empty-token",
            false,
        )
        .await;
        let response = latin1_string(&raw);
        assert!(
            response.starts_with("HTTP/1.1 401"),
            "#1349 an empty token must not admit `{authorization}`: {response}"
        );
    }
}

/// #1349: a gap operation (no process-level Rust owner yet) is the typed 501
/// on a direct call and is never advertised, so through the transport it is
/// the 409 the gate answers first.
#[tokio::test]
async fn gap_operation_is_typed_and_never_advertised() {
    let (parent, ports) = ports_fixture();
    assert!(
        !ports.operations().contains(&"model_prepare".to_owned()),
        "#1349 a gap operation is never advertised"
    );
    let response = ports
        .call("model_prepare", &json("{}"), &principal_fixture())
        .await
        .expect("#1349 the gap operation call resolves");
    assert_eq!(
        response.status, 501,
        "#1349 a gap operation is a typed 501 on a direct call"
    );
    let text = body_text(&response);
    assert!(
        text.contains("PUBLIC_HOST_CAPABILITY_UNAVAILABLE") && text.contains("model_prepare"),
        "#1349 the gap 501 names the operation and the TS code: {text}"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

// ------------------------------------------------------------ process audit

/// The child processes of this test process: one `/proc/<pid>/stat` ppid scan
/// with the `/proc/<pid>/exe` of every child.
#[cfg(target_os = "linux")]
fn child_processes() -> Vec<(i32, Option<PathBuf>)> {
    let me = std::process::id() as i32;
    let mut children = Vec::new();
    let entries = std::fs::read_dir("/proc").expect("#1349 the audit reads /proc");
    for entry in entries {
        let entry = entry.expect("#1349 the audit reads a /proc entry");
        let Some(pid) = entry.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        // After `comm)` come state and ppid.
        let Some(ppid) = rest
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<i32>().ok())
        else {
            continue;
        };
        if ppid == me {
            let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
            children.push((pid, exe));
        }
    }
    children.sort();
    children
}

/// #1349: the module-host surface serves describe and call from the Rust
/// host alone. The audited requests go through a real TCP listener with the
/// native provider (the real SkillManager and the real `config::load`), and
/// the child-process set is the same before and after, with no node
/// executable among them — no TS process participates. The audit runs in an
/// owned rerun of this test binary (the #335 deleted-cwd audit's pattern):
/// this lib shares one process with sibling tests that do spawn children,
/// which would make the before/after comparison race on their activity
/// instead of on this exchange.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn describe_and_call_spawn_no_children() {
    if std::env::var_os("PDRS_MODULE_HOST_AUDIT_CHILD").is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().expect("#1349 the audit rerun binary"),
        )
        .args([
            "--exact",
            "module_host::tests::describe_and_call_spawn_no_children",
            "--nocapture",
            "--test-threads=2",
        ])
        .env("PDRS_MODULE_HOST_AUDIT_CHILD", "1")
        .output()
        .expect("#1349 run the audit in an owned process");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            crate::test_exact_receipt::single_pass(
                &output.status,
                "module_host::tests::describe_and_call_spawn_no_children",
                &stdout
            ),
            "#1349 the audit rerun matched no test:\n{stdout}\n{stderr}"
        );
        return;
    }
    let (parent, ports) = ports_fixture();
    let module_config = Config {
        token: "fixture-token".into(),
        capabilities: Some(Arc::new(ModuleHostResolver(ports))),
    };
    let native_config = native_http::Config {
        token: "fixture-token".into(),
        loopback: true,
        static_root: None,
    };
    let before = child_processes();
    let principal = r#"{"principal":{"pilotDeckUserId":"u1","tenantId":"t1","actorUserId":"a1","agentId":"audit"}}"#;
    let describe = format!(
        "POST /api/module-host/describe HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Length: {}\r\n\r\n{}",
        principal.len(),
        principal
    );
    let (raw, _) = serve_and_capture(
        module_config.clone(),
        native_config.clone(),
        describe.as_bytes(),
        "audit-describe",
        false,
    )
    .await;
    assert!(
        find(
            &normalize(&raw),
            b"{\"operations\":[\"list_general_skills\",\"import_general_skill\",\"list_model_catalog\"]}"
        )
        .is_some(),
        "#1349 the audit describe serves the native ports' operations: {}",
        latin1_string(&raw)
    );
    let call =
        "{\"principal\":{\"pilotDeckUserId\":\"u1\",\"tenantId\":\"t1\",\"actorUserId\":\"a1\",\"agentId\":\"audit\"},\"operation\":\"list_model_catalog\",\"input\":{}}"
            .to_string();
    let request = format!(
        "POST /api/module-host/call HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Length: {}\r\n\r\n{}",
        call.len(),
        call
    );
    let (raw, _) = serve_and_capture(
        module_config,
        native_config,
        request.as_bytes(),
        "audit-call",
        false,
    )
    .await;
    assert!(
        find(&normalize(&raw), b"alpha/main").is_some(),
        "#1349 the audit call serves the native catalog: {}",
        latin1_string(&raw)
    );
    let after = child_processes();
    assert_eq!(
        before, after,
        "#1349 the module-host describe and call spawn no child processes"
    );
    for (pid, exe) in after {
        let exe = exe.map(|path| path.to_string_lossy().into_owned());
        assert!(
            !exe.as_deref()
                .is_some_and(|path| path.to_lowercase().contains("node")),
            "#1349 no child process runs a node executable (pid {pid}: {exe:?})"
        );
    }
    let _ = std::fs::remove_dir_all(&parent);
}

/// #1349: the `/proc` audit is Linux-only; elsewhere it is skipped with this
/// note, like the other parity audits of this crate.
#[cfg(not(target_os = "linux"))]
#[test]
fn process_audit_is_linux_only() {
    eprintln!("SKIP: #1349 the module-host process audit reads Linux /proc");
}

/// #996-style regeneration guard for the fixture; see the fixture README for
/// the pinned Node invocation.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_identically() {
    test_baseline::regenerates_identically("module-host");
}
