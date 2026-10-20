//! `web_fetch` / `web_search` replay against the fixed TypeScript baseline
//! (`tests/fixtures/web-tools/generate.mts`). The real TS tools and this port
//! talk to the same scripted loopback mock (plain HTTP and HTTPS with the
//! throwaway model-stream CA); request records and tool results are compared
//! in order, byte for byte. No external traffic.
#[path = "common/baseline.rs"]
mod baseline;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use pilotdeck_gateway::{
    host::modules::{
        capability::ToolDefinition,
        tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    },
    runtime::{
        model::http::{BodyFailure, HyperModelTransport, ModelHttpTransport, TransportFuture},
        tools::{
            ExecutableTool, ToolCancellation, ToolRunContext,
            builtin::web::{
                CustomProviderConfig, HtmlToMarkdown, MissingTurndown, SecondaryModel,
                SecondaryModelEvent, SecondaryStream, UrlFetcher, WebFetchModelPort, WebFetchTool,
                WebFetchToolOptions, WebSearchOptions, WebSearchTool,
                cache::{FetchedEntry, UrlCache},
                fetch_tool::{
                    SecondaryNext, make_secondary_prompt, validate_web_fetch_input,
                    web_fetch_permission,
                },
                fetcher::shared_cache,
                network::{
                    FetchOptions, HttpRequest, RetryOptions, network_fetch, node_timer_delay_ms,
                },
                preapproved_hosts::{PREAPPROVED_ENTRIES, is_preapproved_url},
                search::web_search_permission,
                url_validation::{is_permitted_redirect, upgrade_http_to_https, validate_url},
            },
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use sha2::{Digest, Sha256};
use std::{
    io::{ErrorKind, Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const ORACLE: &str = include_str!("fixtures/web-tools/oracle.json");
const TLS: &str = include_str!("fixtures/model-stream/tls.json");

fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}
fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().unwrap()
}
fn get<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => &[],
    }
}
fn opt_text(value: &JsValue) -> Option<String> {
    value.as_string().map(|s| s.to_utf8().unwrap())
}
fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        if !matches!(value, JsValue::Undefined) {
            out.insert(key.into(), value);
        }
    }
    JsValue::Object(out)
}

// ---- loopback mock ------------------------------------------------------------

#[derive(Default)]
struct Exchange {
    replies: Vec<JsValue>,
    requests: Vec<JsValue>,
    /// Set after the response headers were flushed (body-cancellation tests).
    headers_sent: bool,
    /// Negative control: overrides every reason phrase.
    reason_override: Option<String>,
}

struct Mock {
    port: u16,
    stop: Arc<AtomicBool>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn start_mock(
    exchange: Arc<Mutex<Exchange>>,
    excluded: Arc<Vec<String>>,
    agents: Arc<Vec<String>>,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Mock {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_nodelay(true).unwrap();
                    let (exchange, excluded, agents, tls) = (
                        exchange.clone(),
                        excluded.clone(),
                        agents.clone(),
                        tls.clone(),
                    );
                    thread::spawn(move || match tls {
                        Some(config) => {
                            let connection = rustls::ServerConnection::new(config).unwrap();
                            serve(
                                rustls::StreamOwned::new(connection, socket),
                                &exchange,
                                &excluded,
                                &agents,
                            );
                        }
                        None => serve(socket, &exchange, &excluded, &agents),
                    });
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("accept: {error}"),
            }
        }
    });
    Mock { port, stop }
}

/// method, path, lower-cased headers (duplicates joined), body.
type Received = (String, String, Vec<(String, String)>, Vec<u8>);

fn read_request(socket: &mut impl Read) -> Option<Received> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match socket.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return None,
        }
    }
    let head = String::from_utf8(head).unwrap();
    let mut lines = head.split("\r\n");
    let mut start = lines.next().unwrap().split(' ');
    let method = start.next().unwrap().to_string();
    let path = start.next().unwrap().to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':').unwrap();
        let (name, value) = (name.to_ascii_lowercase(), value.trim().to_string());
        match headers.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = format!("{}, {value}", entry.1),
            None => headers.push((name, value)),
        }
    }
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .map_or(0, |(_, v)| v.parse().unwrap());
    let mut body = vec![0u8; length];
    socket.read_exact(&mut body).ok()?;
    Some((method, path, headers, body))
}

fn reply_bytes(reply: &JsValue) -> Vec<u8> {
    if let JsValue::Array(repeat) = get(reply, "repeat") {
        let unit = text(&repeat[0]).into_bytes()[0];
        return vec![unit; repeat[1].as_number().unwrap() as usize];
    }
    if let Some(hex) = opt_text(get(reply, "hex")) {
        let hex = hex.replace(' ', "");
        return (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
    }
    opt_text(get(reply, "body"))
        .unwrap_or_default()
        .into_bytes()
}

fn serve(
    mut socket: impl Read + Write,
    exchange: &Mutex<Exchange>,
    excluded: &[String],
    agents: &[String],
) {
    let Some((method, path, headers, body)) = read_request(&mut socket) else {
        return;
    };
    let mut kept: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(name, value)| {
            !excluded.contains(name) && !(name == "user-agent" && agents.contains(value))
        })
        .collect();
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    let record = object(vec![
        ("method", method.as_str().into()),
        ("path", path.as_str().into()),
        (
            "headers",
            JsValue::Array(
                kept.iter()
                    .map(|(n, v)| JsValue::Array(vec![n.as_str().into(), v.as_str().into()]))
                    .collect(),
            ),
        ),
        ("body", String::from_utf8(body).unwrap().as_str().into()),
    ]);
    let (reply, reason_override) = {
        let mut exchange = exchange.lock().unwrap();
        exchange.requests.push(record);
        let index = exchange.requests.len() - 1;
        (
            exchange.replies.get(index).cloned(),
            exchange.reason_override.clone(),
        )
    };
    let Some(reply) = reply else { return };
    let bytes = reply_bytes(&reply);
    let status = get(&reply, "status").as_number().unwrap();
    let reason = reason_override.unwrap_or_else(|| text(get(&reply, "reason")));
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    if let Some(headers) = get(&reply, "headers").as_object() {
        for (name, value) in headers.entries() {
            head.push_str(&format!("{}: {}\r\n", name.to_utf8().unwrap(), text(value)));
        }
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        bytes.len()
    ));
    // Test-only: send the headers, keep the connection open without body
    // bytes for a while (body-cancellation tests). The oracle replay never
    // sets this field.
    let hold_ms = get(&reply, "holdAfterHeadersMs").as_number();
    let _ = socket
        .write_all(head.as_bytes())
        .and_then(|()| socket.flush());
    exchange.lock().unwrap().headers_sent = true;
    if let Some(ms) = hold_ms {
        thread::sleep(Duration::from_secs_f64(ms / 1000.0));
    }
    // The client may stop reading an oversized body; that is not an error here.
    let _ = socket.write_all(&bytes).and_then(|()| socket.flush());
}

fn tls_material() -> (rustls::RootCertStore, Arc<rustls::ServerConfig>) {
    let material = parse_json(TLS).unwrap();
    let ca = CertificateDer::from_pem_slice(text(get(&material, "ca")).as_bytes()).unwrap();
    let cert = CertificateDer::from_pem_slice(text(get(&material, "cert")).as_bytes()).unwrap();
    let key = PrivateKeyDer::from_pem_slice(text(get(&material, "key")).as_bytes()).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    (roots, Arc::new(server))
}

// ---- harness --------------------------------------------------------------------

struct Harness {
    oracle: JsValue,
    exchange: Arc<Mutex<Exchange>>,
    transport: Arc<dyn ModelHttpTransport>,
    origins: Vec<(String, String)>,
    _mocks: (Mock, Mock),
}

impl Harness {
    fn new() -> Self {
        let oracle = baseline::oracle(ORACLE);
        let strings = |key: &str| {
            Arc::new(
                items(get(&oracle, key))
                    .iter()
                    .map(text)
                    .collect::<Vec<_>>(),
            )
        };
        let (excluded, agents) = (strings("transportHeaders"), strings("defaultUserAgents"));
        let exchange = Arc::new(Mutex::new(Exchange::default()));
        let (roots, server) = tls_material();
        let plain = start_mock(exchange.clone(), excluded.clone(), agents.clone(), None);
        let tls = start_mock(exchange.clone(), excluded, agents, Some(server));
        let closed = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut origins = vec![
            (
                format!("https://127.0.0.1:{}", tls.port),
                "$TLS".to_string(),
            ),
            (
                format!("http://127.0.0.1:{}", plain.port),
                "$PLAIN".to_string(),
            ),
            (
                format!("http://127.0.0.1:{}", tls.port),
                "$UPGRADE".to_string(),
            ),
            (format!("http://127.0.0.1:{closed}"), "$CLOSED".to_string()),
        ];
        origins.sort_by_key(|(origin, _)| std::cmp::Reverse(origin.len()));
        Self {
            oracle,
            exchange,
            transport: Arc::new(HyperModelTransport::with_roots(roots)),
            origins,
            _mocks: (plain, tls),
        }
    }
    fn real(&self, value: &JsValue) -> JsValue {
        if matches!(value, JsValue::Undefined) {
            return JsValue::Undefined;
        }
        let mut wire = json(value);
        for (origin, name) in &self.origins {
            wire = wire.replace(name.as_str(), origin);
        }
        parse_json(&wire).unwrap()
    }
    fn scrub(&self, value: &JsValue) -> String {
        let mut wire = json(value);
        for (origin, name) in &self.origins {
            wire = wire.replace(origin.as_str(), name);
        }
        wire
    }
    fn begin(&self, spec: &JsValue, reason_override: Option<&str>) {
        let mut exchange = self.exchange.lock().unwrap();
        exchange.replies = items(&self.real(get(spec, "replies"))).to_vec();
        exchange.requests.clear();
        exchange.headers_sent = false;
        exchange.reason_override = reason_override.map(str::to_string);
    }
    fn requests(&self) -> JsValue {
        JsValue::Array(self.exchange.lock().unwrap().requests.clone())
    }
}

fn random() -> Arc<dyn Fn() -> f64 + Send + Sync> {
    Arc::new(|| 0.5)
}

fn context(
    fields_model: Option<Arc<dyn SecondaryModel>>,
    env: JsValue,
    signal: Option<ToolCancellation>,
) -> ToolRunContext {
    let mut fields = JsObject::new();
    fields.insert("cwd".into(), "/w".into());
    fields.insert("sessionId".into(), "s".into());
    fields.insert("env".into(), env);
    let host = ToolRuntimeContext {
        fields,
        files: HostToolCheckpoint::default().tool_context_state(),
        abort_signal: signal.map(|s| Arc::new(s) as _),
        model: fields_model.map(|m| Arc::new(WebFetchModelPort(m)) as _),
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: None,
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    };
    ToolRunContext::new(Arc::new(host))
}

async fn run(tool: &dyn ExecutableTool, input: JsValue, context: ToolRunContext) -> JsValue {
    match tool.execute(input, context).await {
        Ok(out) => object(vec![(
            "ok",
            object(vec![
                ("content", JsValue::Array(out.content)),
                ("data", out.data.unwrap_or(JsValue::Undefined)),
                (
                    "metadata",
                    out.metadata.map_or(JsValue::Undefined, JsValue::Object),
                ),
            ]),
        )]),
        Err(error) => object(vec![(
            "error",
            object(vec![
                ("code", error.code.as_str().into()),
                ("message", JsValue::String(error.message)),
                (
                    "details",
                    error.details.map_or(JsValue::Undefined, JsValue::Object),
                ),
            ]),
        )]),
    }
}

fn hash_units(value: &JsString) -> JsValue {
    let bytes: Vec<u8> = value.units().iter().flat_map(|u| u.to_le_bytes()).collect();
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    object(vec![
        ("sha256", digest.as_str().into()),
        ("units", JsValue::Number(value.units().len() as f64)),
    ])
}

fn hashed(result: JsValue) -> JsValue {
    let Some(ok) = result.get("ok") else {
        return result;
    };
    let content: Vec<JsValue> = items(get(ok, "content"))
        .iter()
        .map(|item| {
            let mut item = item.as_object().unwrap().clone();
            if let Some(JsValue::String(text)) = item.get("text") {
                let hash = hash_units(text);
                item.insert("text".into(), hash);
            }
            JsValue::Object(item)
        })
        .collect();
    let mut ok = ok.as_object().unwrap().clone();
    ok.insert("content".into(), JsValue::Array(content));
    let mut root = result.as_object().unwrap().clone();
    root.insert("ok".into(), JsValue::Object(ok));
    JsValue::Object(root)
}

/// The scripted secondary model (`makeModel` in the generator).
struct ScriptedModel {
    events: Vec<JsValue>,
    thrown: Option<JsValue>,
    abort: Option<ToolCancellation>,
    requests: Mutex<Vec<JsValue>>,
}

impl SecondaryModel for ScriptedModel {
    fn stream(
        &self,
        request: JsValue,
        _signal: Option<ToolRunContext>,
    ) -> Box<dyn SecondaryStream> {
        self.requests.lock().unwrap().push(request);
        // The generator aborts before yielding each event; the tool checks the
        // signal on every event it receives.
        if let Some(abort) = &self.abort {
            abort.cancel();
        }
        let mut out: Vec<SecondaryModelEvent> = self
            .events
            .iter()
            .cloned()
            .map(SecondaryModelEvent::Event)
            .collect();
        if let Some(thrown) = &self.thrown {
            out.push(if matches!(get(thrown, "error"), JsValue::Bool(true)) {
                let mut value = JsObject::new();
                for (key, item) in thrown.as_object().unwrap().entries() {
                    let key = key.to_utf8().unwrap();
                    match key.as_str() {
                        "error" => {}
                        "error_" => {
                            value.insert("error".into(), item.clone());
                        }
                        _ => {
                            value.insert(key.as_str().into(), item.clone());
                        }
                    }
                }
                SecondaryModelEvent::Throw {
                    is_error: true,
                    value: JsValue::Object(value),
                }
            } else {
                SecondaryModelEvent::Throw {
                    is_error: false,
                    value: get(thrown, "value").clone(),
                }
            });
        }
        Box::new(VecStream {
            events: out.into_iter(),
        })
    }
}

/// The scripted model's async iterable: one event per `next()`.
struct VecStream {
    events: std::vec::IntoIter<SecondaryModelEvent>,
}

impl SecondaryStream for VecStream {
    fn next(&mut self) -> SecondaryNext<'_> {
        let next = self.events.next();
        Box::pin(async move { next })
    }
}

struct FakeConverter;
impl HtmlToMarkdown for FakeConverter {
    fn convert(&self, _html: &str) -> Result<String, String> {
        Ok("# Hi\n\nx".into())
    }
}

#[derive(Default)]
struct Mutation {
    html: Option<Arc<dyn HtmlToMarkdown>>,
    reason: Option<&'static str>,
}

/// One `fetches` scenario; returns (requests, modelRequests, results) scrubbed.
async fn replay_fetch(
    h: &Harness,
    spec: &JsValue,
    mutation: &Mutation,
) -> (String, String, String) {
    h.begin(spec, mutation.reason);
    let cache = Arc::new(UrlCache::new(None));
    if let Some(seed) = get(spec, "cacheSeed").as_object() {
        let content = text(seed.get("content").unwrap());
        let entry = FetchedEntry {
            bytes: content.len(),
            code: 200,
            code_text: "OK".into(),
            content: content.clone(),
            content_type: "text/markdown".into(),
        };
        cache.set(&text(seed.get("url").unwrap()), entry, content.len());
    }
    let signal = ToolCancellation::default();
    let model_spec = get(spec, "model");
    let model: Option<Arc<ScriptedModel>> =
        (!matches!(model_spec, JsValue::Undefined)).then(|| {
            let (events, thrown) = match model_spec {
                JsValue::Array(events) => (events.clone(), None),
                other => (
                    items(get(other, "events")).to_vec(),
                    Some(get(other, "throw").clone()),
                ),
            };
            Arc::new(ScriptedModel {
                events,
                thrown,
                abort: matches!(get(spec, "abortDuringModel"), JsValue::Bool(true))
                    .then(|| signal.clone()),
                requests: Mutex::new(Vec::new()),
            })
        });
    let via_context = opt_text(get(spec, "modelVia")).as_deref() == Some("context");
    let as_model = model.clone().map(|m| m as Arc<dyn SecondaryModel>);
    let fetcher = UrlFetcher {
        transport: h.transport.clone(),
        cache,
        html: mutation
            .html
            .clone()
            .unwrap_or_else(|| Arc::new(MissingTurndown)),
        random: random(),
    };
    let tool = WebFetchTool::new(WebFetchToolOptions {
        model: if via_context { None } else { as_model.clone() },
        provider: None,
        model_id: None,
        max_output_tokens: None,
        fetcher,
    });
    let ctx = context(
        if via_context { as_model } else { None },
        JsValue::Object(JsObject::new()),
        Some(signal),
    );
    let input = h.real(get(spec, "input"));
    let mut results = vec![run(&tool, input.clone(), ctx.clone()).await];
    if matches!(get(spec, "repeatCall"), JsValue::Bool(true)) {
        results.push(run(&tool, input, ctx.clone()).await);
    }
    if !matches!(get(spec, "secondInput"), JsValue::Undefined) {
        results.push(run(&tool, h.real(get(spec, "secondInput")), ctx.clone()).await);
    }
    if matches!(get(spec, "hashOnly"), JsValue::Bool(true)) {
        results = results.into_iter().map(hashed).collect();
    }
    let model_requests = JsValue::Array(
        model
            .map(|m| m.requests.lock().unwrap().clone())
            .unwrap_or_default(),
    );
    (
        h.scrub(&h.requests()),
        h.scrub(&model_requests),
        h.scrub(&JsValue::Array(results)),
    )
}

fn string_option(value: &JsValue, key: &str) -> Option<String> {
    opt_text(get(value, key))
}

fn search_options(transport: Arc<dyn ModelHttpTransport>, options: &JsValue) -> WebSearchOptions {
    let mut out = WebSearchOptions::new(transport);
    out.provider = string_option(options, "provider");
    out.search_engine = string_option(options, "searchEngine");
    out.api_key = string_option(options, "apiKey");
    out.endpoint = string_option(options, "endpoint");
    out.organic_limit = get(options, "organicLimit").as_number();
    out.random = random();
    let custom = get(options, "customProvider");
    if custom.as_object().is_some() {
        out.custom_provider = Some(CustomProviderConfig {
            name: string_option(custom, "name"),
            auth: string_option(custom, "auth"),
            method: string_option(custom, "method"),
            query_param: string_option(custom, "queryParam"),
            api_key_param: string_option(custom, "apiKeyParam"),
            results_path: string_option(custom, "resultsPath"),
            title_field: string_option(custom, "titleField"),
            url_field: string_option(custom, "urlField"),
            snippet_field: string_option(custom, "snippetField"),
            source_field: string_option(custom, "sourceField"),
            published_at_field: string_option(custom, "publishedAtField"),
        });
    }
    out
}

async fn replay_search(h: &Harness, spec: &JsValue) -> (String, String) {
    h.begin(spec, None);
    let tool = WebSearchTool::new(search_options(
        h.transport.clone(),
        &h.real(get(spec, "options")),
    ));
    let env = match get(spec, "env") {
        JsValue::Undefined => JsValue::Object(JsObject::new()),
        env => h.real(env),
    };
    let result = run(&tool, get(spec, "input").clone(), context(None, env, None)).await;
    (h.scrub(&h.requests()), h.scrub(&result))
}

fn expect(spec: &JsValue, key: &str) -> String {
    json(get(spec, key))
}

/// Replay the exact boundary oracle without listening on a socket, so sandbox
/// restrictions cannot hide a MAX_REDIRECTS off-by-one mutation.
#[tokio::test]
async fn real_ts_redirect_exact_boundary_without_sockets() {
    struct BoundaryTransport {
        replies: Vec<JsValue>,
        requests: Arc<Mutex<Vec<JsValue>>>,
    }
    impl ModelHttpTransport for BoundaryTransport {
        fn send(&self, request: hyper::Request<Full<Bytes>>) -> TransportFuture {
            let mut headers = request
                .headers()
                .iter()
                .map(|(key, value)| (key.as_str(), value.to_str().unwrap()))
                .collect::<Vec<_>>();
            headers.sort();
            let record = object(vec![
                ("method", request.method().as_str().into()),
                (
                    "path",
                    request.uri().path_and_query().unwrap().as_str().into(),
                ),
                (
                    "headers",
                    JsValue::Array(
                        headers
                            .into_iter()
                            .map(|(key, value)| JsValue::Array(vec![key.into(), value.into()]))
                            .collect(),
                    ),
                ),
                ("body", "".into()),
            ]);
            let mut requests = self.requests.lock().unwrap();
            let reply = &self.replies[requests.len()];
            requests.push(record);
            let mut response =
                hyper::Response::builder().status(get(reply, "status").as_number().unwrap() as u16);
            for (name, value) in get(reply, "headers").as_object().unwrap().entries() {
                response = response.header(name.to_utf8().unwrap(), text(value));
            }
            TransportFuture::ready(Ok(response
                .body(
                    Full::new(Bytes::from(reply_bytes(reply)))
                        .map_err(|never| -> BodyFailure { match never {} })
                        .boxed(),
                )
                .unwrap()))
        }
    }
    let oracle = baseline::oracle(ORACLE);
    for name in [
        "redirect-exactly-ten-hops",
        "redirect-eleventh-hop-rejected",
    ] {
        let original = items(get(&oracle, "fetches"))
            .iter()
            .find(|spec| text(get(spec, "name")) == name)
            .unwrap();
        let spec = parse_json(&json(original).replace("$TLS", "https://example.com")).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(BoundaryTransport {
            replies: items(get(&spec, "replies")).to_vec(),
            requests: requests.clone(),
        });
        let tool = WebFetchTool::new(WebFetchToolOptions {
            fetcher: UrlFetcher {
                transport,
                cache: Arc::new(UrlCache::new(None)),
                html: Arc::new(MissingTurndown),
                random: random(),
            },
            model: None,
            provider: None,
            model_id: None,
            max_output_tokens: None,
        });
        let result = run(
            &tool,
            get(&spec, "input").clone(),
            context(None, empty_env(), None),
        )
        .await;
        assert_eq!(
            json(&JsValue::Array(vec![result])),
            expect(&spec, "results"),
            "{name}: results"
        );
        assert_eq!(
            json(&JsValue::Array(requests.lock().unwrap().clone())),
            expect(&spec, "requests"),
            "{name}: requests"
        );
        assert_eq!(requests.lock().unwrap().len(), 11);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ts_web_fetch_and_web_search_over_loopback() {
    let h = Harness::new();
    let fetches = items(get(&h.oracle, "fetches"));
    let searches = items(get(&h.oracle, "searches"));
    assert_eq!((fetches.len(), searches.len()), (43, 39), "scenario counts");
    let mut requests = 0;
    for spec in fetches {
        let name = text(get(spec, "name"));
        let (reqs, model_requests, results) = replay_fetch(&h, spec, &Mutation::default()).await;
        assert_eq!(reqs, expect(spec, "requests"), "{name}: requests");
        assert_eq!(
            model_requests,
            expect(spec, "modelRequests"),
            "{name}: model requests"
        );
        assert_eq!(results, expect(spec, "results"), "{name}: results");
        assert_eq!(
            items(get(spec, "requests")).len(),
            items(get(spec, "replies")).len(),
            "{name}: replies consumed"
        );
        requests += items(get(spec, "requests")).len();
    }
    for spec in searches {
        let name = text(get(spec, "name"));
        let (reqs, result) = replay_search(&h, spec).await;
        assert_eq!(reqs, expect(spec, "requests"), "{name}: requests");
        assert_eq!(result, expect(spec, "result"), "{name}: result");
        requests += items(get(spec, "requests")).len();
    }
    assert_eq!(requests, 112, "total scripted requests");
}

#[test]
fn real_ts_pure_helpers_tools_and_cache() {
    let oracle = baseline::oracle(ORACLE);
    let urls = items(get(&oracle, "urls"));
    assert_eq!(urls.len(), 20);
    for case in urls {
        let url = text(get(case, "url"));
        assert_eq!(
            JsValue::Bool(validate_url(&url)),
            *get(case, "valid"),
            "validateURL({url})"
        );
        assert_eq!(
            JsValue::Bool(is_preapproved_url(&url)),
            *get(case, "preapproved"),
            "isPreapprovedUrl({url})"
        );
    }
    let upgrades = items(get(&oracle, "upgrades"));
    assert_eq!(upgrades.len(), 9);
    for case in upgrades {
        let url = text(get(case, "url"));
        assert_eq!(
            upgrade_http_to_https(&url),
            opt_text(get(case, "upgraded")),
            "upgrade({url})"
        );
    }
    let redirects = items(get(&oracle, "redirects"));
    assert_eq!(redirects.len(), 10);
    for case in redirects {
        let (from, to) = (text(get(case, "from")), text(get(case, "to")));
        assert_eq!(
            JsValue::Bool(is_permitted_redirect(&from, &to)),
            *get(case, "permitted"),
            "{from} -> {to}"
        );
    }
    let preapproved = items(get(&oracle, "preapproved"));
    assert_eq!(preapproved.len(), 14);
    for case in preapproved {
        let url = text(get(case, "url"));
        assert_eq!(
            JsValue::Bool(is_preapproved_url(&url)),
            *get(case, "preapproved"),
            "{url}"
        );
    }
    // The whole table, not a sample: any transcribed drift is a false PASS.
    let entries = items(get(&oracle, "preapprovedEntries"));
    assert_eq!(entries.len(), PREAPPROVED_ENTRIES.len(), "entry count");
    for (entry, expected) in PREAPPROVED_ENTRIES.iter().zip(entries) {
        assert_eq!(entry.host, text(get(expected, "host")));
        assert_eq!(
            entry.path_prefix.map(str::to_string),
            opt_text(get(expected, "pathPrefix"))
        );
        assert_eq!(
            entry.allow_subdomains,
            matches!(get(expected, "allowSubdomains"), JsValue::Bool(true)),
        );
    }
    let prompts = items(get(&oracle, "prompts"));
    assert_eq!(prompts.len(), 3);
    for case in prompts {
        let md = get(case, "md").as_string().unwrap();
        let prompt = get(case, "prompt").as_string().unwrap();
        let pre = matches!(get(case, "pre"), JsValue::Bool(true));
        assert_eq!(
            JsValue::String(make_secondary_prompt(md, prompt, pre)),
            *get(case, "text")
        );
    }
    let validations = items(get(&oracle, "validations"));
    assert_eq!(validations.len(), 15);
    for case in validations {
        let input = get(case, "input").clone();
        let result = match validate_web_fetch_input(&input) {
            None => object(vec![("ok", JsValue::Bool(true)), ("input", input)]),
            Some(issues) => object(vec![
                ("ok", JsValue::Bool(false)),
                (
                    "issues",
                    JsValue::Array(issues.into_iter().map(JsValue::Object).collect()),
                ),
            ]),
        };
        assert_eq!(
            json(&result),
            expect(case, "result"),
            "validateInput({})",
            expect(case, "input")
        );
    }

    // Tool definitions and permission requests.
    let transport: Arc<dyn ModelHttpTransport> = Arc::new(HyperModelTransport::new());
    let fetch = WebFetchTool::new(WebFetchToolOptions {
        model: None,
        provider: None,
        model_id: None,
        max_output_tokens: None,
        fetcher: UrlFetcher::new(transport.clone()),
    });
    let search = WebSearchTool::new(WebSearchOptions::new(transport.clone()));
    for (key, tool) in [
        ("webFetch", &fetch as &dyn ToolDefinition),
        ("webSearch", &search as &dyn ToolDefinition),
    ] {
        let expected = get(get(&oracle, "tools"), key);
        for field in [
            "name",
            "aliases",
            "description",
            "kind",
            "inputSchema",
            "maxResultBytes",
        ] {
            assert_eq!(
                json(tool.fields().get(field).unwrap()),
                expect(expected, field),
                "{key}.{field}"
            );
        }
        assert_eq!(
            tool.is_read_only(&JsValue::Undefined).unwrap(),
            *get(expected, "isReadOnly")
        );
        assert_eq!(
            tool.is_concurrency_safe(&JsValue::Undefined).unwrap(),
            *get(expected, "isConcurrencySafe")
        );
    }
    let permissions = get(&oracle, "permissions");
    assert_eq!(
        json(&web_fetch_permission()),
        expect(permissions, "webFetch")
    );
    assert_eq!(
        json(&web_search_permission()),
        expect(permissions, "webSearch")
    );

    let availability = items(get(&oracle, "availability"));
    assert_eq!(availability.len(), 6);
    for case in availability {
        let tool = WebSearchTool::new(search_options(transport.clone(), get(case, "options")));
        let result = match tool.check_availability(Some(get(case, "env"))) {
            None => object(vec![("ok", JsValue::Bool(true))]),
            Some((code, reason)) => object(vec![
                ("ok", JsValue::Bool(false)),
                ("code", code.into()),
                ("reason", reason.into()),
            ]),
        };
        assert_eq!(
            json(&result),
            expect(case, "result"),
            "checkAvailability({})",
            expect(case, "options")
        );
    }

    // WebFetchUrlCache with a pinned clock.
    let now = Arc::new(Mutex::new(0.0));
    let clock = now.clone();
    let cache = UrlCache::new(Some(Arc::new(move || *clock.lock().unwrap())));
    let ops = items(get(&oracle, "cacheOps"));
    assert_eq!(ops.len(), 17);
    for op in ops {
        *now.lock().unwrap() = get(op, "at").as_number().unwrap();
        let mut result = JsValue::Null;
        match text(get(op, "op")).as_str() {
            "set" => {
                let entry = FetchedEntry {
                    bytes: 1,
                    code: 200,
                    code_text: "OK".into(),
                    content: text(get(op, "content")),
                    content_type: "text/plain".into(),
                };
                cache.set(
                    &text(get(op, "url")),
                    entry,
                    get(op, "bytes").as_number().unwrap() as usize,
                );
            }
            "get" => {
                if let Some(entry) = cache.get(&text(get(op, "url"))) {
                    result = entry.content.as_str().into();
                }
            }
            _ => cache.clear(),
        }
        assert_eq!(result, *get(op, "result"), "cache op {}", json(op));
        let keys: Vec<String> = items(get(op, "keys")).iter().map(text).collect();
        assert_eq!(cache.urls(), keys, "cache keys after {}", json(op));
        assert_eq!(
            cache.total_bytes() as f64,
            get(op, "totalBytes").as_number().unwrap(),
            "cache bytes after {}",
            json(op)
        );
    }
    // The process-wide cache is one shared instance, like URL_CACHE.
    assert!(Arc::ptr_eq(&shared_cache(), &shared_cache()));
}

fn scenario<'a>(h: &'a Harness, name: &str) -> &'a JsValue {
    items(get(&h.oracle, "fetches"))
        .iter()
        .find(|s| text(get(s, "name")) == name)
        .unwrap()
}

/// Negative control: a working HTML converter (what turndown would do) must
/// make the turndown-missing scenario diverge from the recorded TS failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negative_control_html_converter_is_detected() {
    let h = Harness::new();
    let spec = scenario(&h, "html-turndown-missing");
    let mutation = Mutation {
        html: Some(Arc::new(FakeConverter)),
        ..Default::default()
    };
    let (_, _, results) = replay_fetch(&h, spec, &mutation).await;
    assert_ne!(results, expect(spec, "results"));
    assert!(results.contains("# Hi"), "{results}");
}

/// Negative control: a different reason phrase from the server must change
/// the status text the replay sees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negative_control_reason_phrase_is_detected() {
    let h = Harness::new();
    let spec = scenario(&h, "http-custom-reason");
    let mutation = Mutation {
        reason: Some("Mock"),
        ..Default::default()
    };
    let (requests, _, results) = replay_fetch(&h, spec, &mutation).await;
    assert_eq!(requests, expect(spec, "requests"));
    assert_ne!(results, expect(spec, "results"));
    assert!(results.contains("400 Mock"), "{results}");
}

// ---- body cancellation / secondary stream (fix round 1) --------------------

/// One reply that sends the headers and then holds the body back for two
/// seconds. Not used by the oracle replay.
fn hold_spec() -> JsValue {
    object(vec![(
        "replies",
        JsValue::Array(vec![object(vec![
            ("status", JsValue::Number(200.0)),
            ("reason", JsValue::from("OK")),
            (
                "headers",
                object(vec![("content-type", JsValue::from("text/markdown"))]),
            ),
            ("body", JsValue::from("late")),
            ("holdAfterHeadersMs", JsValue::Number(2_000.0)),
        ])]),
    )])
}

fn plain_fetcher(h: &Harness) -> UrlFetcher {
    UrlFetcher {
        transport: h.transport.clone(),
        cache: Arc::new(UrlCache::new(None)),
        html: Arc::new(MissingTurndown),
        random: random(),
    }
}

fn empty_env() -> JsValue {
    JsValue::Object(JsObject::new())
}

/// Cancel only after the mock flushed its headers: the cancellation then
/// hits the body read, not the send.
async fn cancel_after_headers(h: &Harness, signal: &ToolCancellation) {
    for _ in 0..400 {
        if h.exchange.lock().unwrap().headers_sent {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal.cancel();
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("mock never sent response headers");
}

/// The body read races the parent signal: a server that sent headers and then
/// stalls must not hold the tool call after the user cancels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_fetch_body_read_observes_parent_cancellation() {
    let h = Harness::new();
    h.begin(&hold_spec(), None);
    let tool = WebFetchTool::new(WebFetchToolOptions {
        model: None,
        provider: None,
        model_id: None,
        max_output_tokens: None,
        fetcher: plain_fetcher(&h),
    });
    let signal = ToolCancellation::default();
    let context = context(None, empty_env(), Some(signal.clone()));
    let input = h.real(&object(vec![
        ("url", JsValue::from("$TLS/hold")),
        ("mode", JsValue::from("raw")),
    ]));
    let started = Instant::now();
    let (result, ()) = tokio::join!(
        run(&tool, input, context),
        cancel_after_headers(&h, &signal)
    );
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "body read ignored the parent signal: {:?}",
        started.elapsed()
    );
    // The request was recorded, i.e. the headers had already arrived; the
    // cancellation therefore hit the body read, not the send.
    assert_eq!(items(&h.requests()).len(), 1, "headers reached the mock");
    let error = get(&result, "error");
    assert_eq!(text(get(error, "code")), "tool_execution_failed");
    assert!(
        text(get(error, "message")).contains("Network request aborted by parent signal."),
        "{}",
        json(&result)
    );
}

/// Negative control for the test above: without a cancellation the very same
/// reply keeps the tool waiting, so the prompt return above is the signal's
/// doing and not a body that the mock had already finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negative_control_body_stall_without_cancellation() {
    let h = Harness::new();
    h.begin(&hold_spec(), None);
    let tool = WebFetchTool::new(WebFetchToolOptions {
        model: None,
        provider: None,
        model_id: None,
        max_output_tokens: None,
        fetcher: plain_fetcher(&h),
    });
    let input = h.real(&object(vec![
        ("url", JsValue::from("$TLS/hold")),
        ("mode", JsValue::from("raw")),
    ]));
    let result = tokio::time::timeout(
        Duration::from_millis(400),
        run(&tool, input, context(None, empty_env(), None)),
    )
    .await;
    assert!(result.is_err(), "the mock must still be holding the body");
}

/// `web_search` reads its body through the same helper; cancelling mid-body
/// must settle the provider call too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_body_read_observes_parent_cancellation() {
    let h = Harness::new();
    h.begin(&hold_spec(), None);
    let options = h.real(&object(vec![
        ("provider", JsValue::from("glm")),
        ("apiKey", JsValue::from("k")),
        ("endpoint", JsValue::from("$PLAIN/hold")),
    ]));
    let tool = WebSearchTool::new(search_options(h.transport.clone(), &options));
    let signal = ToolCancellation::default();
    let context = context(None, empty_env(), Some(signal.clone()));
    let started = Instant::now();
    let (result, ()) = tokio::join!(
        run(&tool, object(vec![("query", JsValue::from("q"))]), context),
        cancel_after_headers(&h, &signal)
    );
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "body read ignored the parent signal: {:?}",
        started.elapsed()
    );
    assert_eq!(items(&h.requests()).len(), 1, "headers reached the mock");
    let error = get(&result, "error");
    assert_eq!(text(get(error, "code")), "tool_execution_failed");
    assert!(
        text(get(error, "message")).contains("Network request aborted by parent signal."),
        "{}",
        json(&result)
    );
}

// ---- timeoutMs boundaries / cancellation order (fix round 2) ---------------

/// One reply that the transport returns after `delay`, recording that the
/// request was really started (the outer timer must abort an issued call, not
/// skip it).
struct DelayedTransport {
    delay: Duration,
    calls: Arc<Mutex<usize>>,
}
impl ModelHttpTransport for DelayedTransport {
    fn send(&self, _request: hyper::Request<Full<Bytes>>) -> TransportFuture {
        *self.calls.lock().unwrap() += 1;
        TransportFuture::after(self.delay, Ok(search_response(200)))
    }
}

fn search_response(
    status: u16,
) -> hyper::Response<pilotdeck_gateway::runtime::model::http::ModelBody> {
    let body = Full::new(Bytes::from_static(br#"{"search_result":[]}"#))
        .map_err(|never: std::convert::Infallible| -> BodyFailure { match never {} })
        .boxed();
    hyper::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("retry-after", "0.05")
        .body(body)
        .unwrap()
}

fn delayed_search(timeout_ms: f64, delay: Duration) -> (WebSearchTool, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0));
    let transport: Arc<dyn ModelHttpTransport> = Arc::new(DelayedTransport {
        delay,
        calls: calls.clone(),
    });
    let mut options = WebSearchOptions::new(transport);
    options.provider = Some("glm".into());
    options.api_key = Some("k".into());
    options.timeout_ms = Some(timeout_ms);
    options.random = random();
    (WebSearchTool::new(options), calls)
}

/// Node's `setTimeout(..., timeoutMs)` is unconditional: 0, a negative value
/// and NaN are clamped to an immediate timer and abort the provider call,
/// while `networkFetch`'s own retry timer stays disabled. The baseline probe
/// (`probes/search-timeout.out`) returns `tool_timeout` with the raw value in
/// the message for all three. The paused clock keeps the race with the
/// transport's 60 ms sleep deterministic.
#[tokio::test(start_paused = true)]
async fn web_search_non_positive_timeout_still_arms_the_outer_timer() {
    for (timeout_ms, shown) in [(0.0, "0"), (-1.0, "-1"), (f64::NAN, "NaN")] {
        let (tool, calls) = delayed_search(timeout_ms, Duration::from_millis(60));
        let result = run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), None),
        )
        .await;
        let error = get(&result, "error");
        assert_eq!(
            text(get(error, "code")),
            "tool_timeout",
            "timeoutMs={shown}: {}",
            json(&result)
        );
        assert_eq!(
            text(get(error, "message")),
            format!("web_search timed out after {shown}ms."),
            "timeoutMs={shown}"
        );
        assert_eq!(*calls.lock().unwrap(), 1, "timeoutMs={shown}");
    }
}

/// Negative control: the same 60 ms transport with a positive budget returns
/// the search result, so the three timeouts above are the timer's doing.
#[tokio::test(start_paused = true)]
async fn negative_control_positive_timeout_lets_delayed_response_through() {
    let (tool, calls) = delayed_search(100.0, Duration::from_millis(60));
    let result = run(
        &tool,
        object(vec![("query", JsValue::from("q"))]),
        context(None, empty_env(), None),
    )
    .await;
    let ok = get(&result, "ok");
    assert_eq!(json(get(ok, "data")), r#"{"query":"q","organic":[]}"#);
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// `Duration::from_secs_f64` panics on non-finite or overflowing seconds.
/// Node's `setTimeout` instead truncates valid fractional delays, and turns
/// `NaN`, `±Infinity` and everything
/// outside it into 1 ms (`probes/timer-clamp.out`).
#[test]
fn node_set_timeout_clamp_boundaries() {
    let cases = [
        (0.0, 1.0),
        (0.5, 1.0),
        (0.9, 1.0),
        (-1.0, 1.0),
        (f64::NAN, 1.0),
        (f64::NEG_INFINITY, 1.0),
        (1.0, 1.0),
        (1.9, 1.0),
        (2.0, 2.0),
        (60.0, 60.0),
        (60.9, 60.0),
        (i32::MAX as f64, i32::MAX as f64),
        (i32::MAX as f64 + 0.5, 1.0),
        (i32::MAX as f64 + 1.0, 1.0),
        (1e23, 1.0),
        (f64::INFINITY, 1.0),
    ];
    for (requested, armed) in cases {
        assert_eq!(
            node_timer_delay_ms(requested),
            armed,
            "requested {requested}"
        );
    }
}

/// A transport that never settles: only a timer can end the call.
struct NeverTransport;
impl ModelHttpTransport for NeverTransport {
    fn send(&self, _request: hyper::Request<Full<Bytes>>) -> TransportFuture {
        TransportFuture::io(std::future::pending())
    }
}

fn network_fetch_options(timeout_ms: f64) -> FetchOptions<'static> {
    FetchOptions {
        timeout_ms,
        parent: None,
        retry: RetryOptions {
            max_retries: 0,
            base_delay_ms: 1.0,
            max_delay_ms: 1.0,
            retry_on_post: false,
        },
        random: random(),
    }
}

/// Regression for the panic on `network.rs`'s per-attempt timer: `Infinity`
/// and `1e23` must produce Node's 1 ms timeout, not reach
/// `Duration::from_secs_f64`. The 2^31 case is the clamp boundary Node warns
/// about (`TimeoutOverflowWarning ... set to 1`).
#[tokio::test(start_paused = true)]
async fn network_fetch_clamps_non_finite_and_overflowing_timeout() {
    let transport = NeverTransport;
    let request = HttpRequest {
        method: "GET",
        url: "http://example.com/".into(),
        headers: Vec::new(),
        body: None,
    };
    for (timeout_ms, shown) in [
        (f64::INFINITY, "Infinity"),
        (1e23, "1e+23"),
        (i32::MAX as f64 + 1.0, "2147483648"),
    ] {
        let options = network_fetch_options(timeout_ms);
        let error = network_fetch(&transport, &request, &options)
            .await
            .err()
            .expect("the timer must fire");
        assert_eq!(error.code, "network_timeout", "timeoutMs={shown}");
        assert_eq!(
            error.message.to_utf8().unwrap(),
            format!("Network request timed out after {shown}ms."),
            "timeoutMs={shown}"
        );
    }
}

/// Negative control for the clamp boundary: `2^31-1` ms is still a valid
/// delay (Node only overflows above it), so a 60 ms response is not collapsed
/// into an immediate 1 ms timeout.
#[tokio::test(start_paused = true)]
async fn negative_control_max_valid_timeout_lets_response_through() {
    let (tool, calls) = delayed_search(i32::MAX as f64, Duration::from_millis(60));
    let result = run(
        &tool,
        object(vec![("query", JsValue::from("q"))]),
        context(None, empty_env(), None),
    )
    .await;
    let ok = get(&result, "ok");
    assert_eq!(json(get(ok, "data")), r#"{"query":"q","organic":[]}"#);
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// The reported P1: a huge `timeoutMs` used to panic in the inner timer. The
/// baseline (`probes/search-timeout-block.out`) reports `tool_timeout` with
/// the raw value for both `Infinity` and `1e23`, because Node clamps both to
/// 1 ms.
#[tokio::test(start_paused = true)]
async fn web_search_infinite_or_overflowing_timeout_is_clamped_not_panicking() {
    for (timeout_ms, shown) in [(f64::INFINITY, "Infinity"), (1e23, "1e+23")] {
        let (tool, calls) = delayed_search(timeout_ms, Duration::from_millis(60));
        let result = run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), None),
        )
        .await;
        let error = get(&result, "error");
        assert_eq!(
            text(get(error, "code")),
            "tool_timeout",
            "timeoutMs={shown}: {}",
            json(&result)
        );
        assert_eq!(
            text(get(error, "message")),
            format!("web_search timed out after {shown}ms."),
            "timeoutMs={shown}"
        );
        assert_eq!(*calls.lock().unwrap(), 1, "timeoutMs={shown}");
    }
}

/// The reviewer's starvation case, made deterministic with the paused clock:
/// the 1 ms abort timer and the transport's 60 ms timer are both already due
/// when the tool is polled again. The baseline keeps their order — the abort
/// wins — so an expired timeout must not be replaced by the late response.
/// `negative_control_max_valid_timeout_lets_response_through` shows the
/// opposite order (response first, 60 < 100).
#[tokio::test(start_paused = true)]
async fn web_search_expired_timeout_is_not_replaced_by_a_later_response() {
    let (tool, calls) = delayed_search(1.0, Duration::from_millis(60));
    let driver = {
        let calls = calls.clone();
        async move {
            while *calls.lock().unwrap() == 0 {
                tokio::task::yield_now().await;
            }
            // `send` recorded the call before the transport future was
            // polled; one more yield so the 60 ms sleep and both outer select
            // branches have been registered.
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(120)).await;
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            run(
                &tool,
                object(vec![("query", JsValue::from("q"))]),
                context(None, empty_env(), None),
            ),
            driver
        )
    })
    .await
    .expect("the tool settles");
    let error = get(&result, "error");
    assert_eq!(
        text(get(error, "code")),
        "tool_timeout",
        "{}",
        json(&result)
    );
    assert_eq!(
        text(get(error, "message")),
        "web_search timed out after 1ms."
    );
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// Poll exactly once, then let the driver advance the clock without polling
/// the tool. Unlike spawning two tasks, this cannot accidentally process the
/// response before both timers are due.
async fn poll_pending<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending(), "must have yielded");
        std::task::Poll::Ready(())
    })
    .await;
}

/// TS Node probe: overdue timers retain their due-time ordering and the outer
/// timer was registered first in a tie. The 100/60 case is the negative
/// control for unconditional timeout priority; 1/60 rejects response priority.
#[tokio::test(start_paused = true)]
async fn web_search_orders_both_expired_response_and_timeout() {
    for (budget, success) in [(1.0, false), (60.0, false), (100.0, true)] {
        let (tool, calls) = delayed_search(budget, Duration::from_millis(60));
        let mut pending = Box::pin(run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), None),
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(120)).await;
        let result = pending.await;
        if success {
            assert_eq!(
                json(get(get(&result, "ok"), "data")),
                r#"{"query":"q","organic":[]}"#
            );
        } else {
            assert_eq!(text(get(get(&result, "error"), "code")), "tool_timeout");
        }
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}

/// A delivered parent abort wins even if the loop was blocked past the timeout
/// before abort() was called. Nominal deadlines are not delivered callbacks.
#[tokio::test(start_paused = true)]
async fn web_search_parent_abort_is_not_overwritten_by_overdue_timers() {
    for abort_after_block in [false, true] {
        let (tool, calls) = delayed_search(100.0, Duration::from_millis(60));
        let signal = ToolCancellation::default();
        let mut pending = Box::pin(run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), Some(signal.clone())),
        ));
        poll_pending(pending.as_mut()).await;
        if !abort_after_block {
            signal.cancel();
        }
        tokio::time::advance(Duration::from_millis(120)).await;
        if abort_after_block {
            // Explicitly model JS abort() in the blocked call stack, not a
            // later timer callback. Timestamp alone cannot distinguish them.
            signal.cancel_synchronously();
        }
        let result = pending.await;
        let error = get(&result, "error");
        assert_eq!(text(get(error, "code")), "tool_execution_failed");
        assert_eq!(
            text(get(error, "message")),
            "web_search (glm) request failed: Network request aborted by parent signal."
        );
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}

/// Negative control for parent priority: dispatching a response/timeout first
/// settles the call; a subsequently delivered abort cannot replace that result.
#[tokio::test(start_paused = true)]
async fn web_search_later_parent_abort_preserves_settled_event() {
    for (budget, success) in [(30.0, false), (100.0, true)] {
        let (tool, _) = delayed_search(budget, Duration::from_millis(60));
        let signal = ToolCancellation::default();
        let mut pending = Box::pin(run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), Some(signal.clone())),
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(60)).await;
        let result = pending.await;
        signal.cancel();
        tokio::time::advance(Duration::from_millis(60)).await;
        if success {
            assert_eq!(
                json(get(get(&result, "ok"), "data")),
                r#"{"query":"q","organic":[]}"#
            );
        } else {
            assert_eq!(text(get(get(&result, "error"), "code")), "tool_timeout");
        }
    }
}

/// Standalone networkFetch (also used by web_fetch) must use the same ordering;
/// fixing only the search wrapper would leave its hidden timeout race intact.
#[tokio::test(start_paused = true)]
async fn network_fetch_orders_both_expired_response_and_timeout() {
    for (budget, success) in [(1.0, false), (60.9, false), (100.0, true)] {
        let transport = DelayedTransport {
            delay: Duration::from_millis(60),
            calls: Arc::new(Mutex::new(0)),
        };
        let request = HttpRequest {
            method: "GET",
            url: "http://example.com/".into(),
            headers: Vec::new(),
            body: None,
        };
        let options = network_fetch_options(budget);
        let mut pending = Box::pin(network_fetch(&transport, &request, &options));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(120)).await;
        match pending.await {
            Ok(response) => assert!(success && response.status == 200),
            Err(error) => {
                assert!(!success);
                assert_eq!(error.code, "network_timeout");
            }
        }
    }
}

/// `60.9` schedules in Node's 60 ms timer list, while `61.1` schedules at
/// 61 ms. Checking only Timeout._idleTimeout would make the first case green
/// for the wrong reason. The second is a boundary negative control.
#[tokio::test(start_paused = true)]
async fn web_search_fractional_timer_matches_node_scheduling() {
    for (budget, success) in [(60.9, false), (61.1, true), (2147483647.5, false)] {
        let (tool, _) = delayed_search(budget, Duration::from_millis(60));
        let mut pending = Box::pin(run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), None),
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(120)).await;
        let result = pending.await;
        if success {
            assert_eq!(
                json(get(get(&result, "ok"), "data")),
                r#"{"query":"q","organic":[]}"#
            );
        } else {
            let error = get(&result, "error");
            assert_eq!(text(get(error, "code")), "tool_timeout");
            assert_eq!(
                text(get(error, "message")),
                format!("web_search timed out after {budget}ms.")
            );
        }
    }
}

struct RetryTransport {
    calls: Arc<Mutex<usize>>,
    delayed_second: bool,
}

impl ModelHttpTransport for RetryTransport {
    fn send(&self, _: hyper::Request<Full<Bytes>>) -> TransportFuture {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            TransportFuture::ready(Ok(search_response(429)))
        } else if self.delayed_second {
            TransportFuture::after(Duration::from_millis(60), Ok(search_response(200)))
        } else {
            TransportFuture::ready(Ok(search_response(200)))
        }
    }
}

/// After an overdue retry callback, Node runs microtasks before the next
/// timer. A synchronous second response wins; a pending second fetch times
/// out. Both must issue the second request (probe records calls=2).
#[tokio::test(start_paused = true)]
async fn web_search_overdue_retry_runs_before_later_timeout() {
    for delayed_second in [false, true] {
        let calls = Arc::new(Mutex::new(0));
        let mut options = WebSearchOptions::new(Arc::new(RetryTransport {
            calls: calls.clone(),
            delayed_second,
        }));
        options.provider = Some("glm".into());
        options.api_key = Some("k".into());
        options.timeout_ms = Some(100.0);
        options.random = random();
        let tool = WebSearchTool::new(options);
        let mut pending = Box::pin(run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), None),
        ));
        poll_pending(pending.as_mut()).await;
        assert_eq!(*calls.lock().unwrap(), 1);
        tokio::time::advance(Duration::from_millis(120)).await;
        let result = pending.await;
        if delayed_second {
            assert_eq!(text(get(get(&result, "error"), "code")), "tool_timeout");
        } else {
            assert_eq!(
                json(get(get(&result, "ok"), "data")),
                r#"{"query":"q","organic":[]}"#
            );
        }
        assert_eq!(*calls.lock().unwrap(), 2);
    }
}

struct MicrotaskTransport;
impl ModelHttpTransport for MicrotaskTransport {
    fn send(&self, _: hyper::Request<Full<Bytes>>) -> TransportFuture {
        TransportFuture::microtask(async {
            tokio::task::yield_now().await;
            Ok(search_response(200))
        })
    }
}

/// A queued promise continuation is not a timer: queueMicrotask(resolve) in
/// Node succeeds even with a 1 ms timeout and a 120 ms blocked event loop.
#[tokio::test(start_paused = true)]
async fn web_search_queued_microtask_precedes_overdue_timer() {
    let mut options = WebSearchOptions::new(Arc::new(MicrotaskTransport));
    options.provider = Some("glm".into());
    options.api_key = Some("k".into());
    options.timeout_ms = Some(1.0);
    let tool = WebSearchTool::new(options);
    let mut pending = Box::pin(run(
        &tool,
        object(vec![("query", JsValue::from("q"))]),
        context(None, empty_env(), None),
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(120)).await;
    let result = pending.await;
    assert_eq!(
        json(get(get(&result, "ok"), "data")),
        r#"{"query":"q","organic":[]}"#
    );
}

// ---- round 5: timestamped parent events vs synchronous abort ---------------

/// Oracle: probes/r5-parent-order.out (actual TS search + networkFetch).
/// Parent timer is registered after the response/local timers, including ties.
/// Both operations remain unpolled while every event becomes due.
async fn parent_event_case(budget: f64, response_ms: u64, parent_ms: u64, expected: &str) {
    let (tool, calls) = delayed_search(budget, Duration::from_millis(response_ms));
    let signal = ToolCancellation::default();
    let started = tokio::time::Instant::now();
    let mut pending = Box::pin(run(
        &tool,
        object(vec![("query", JsValue::from("q"))]),
        context(None, empty_env(), Some(signal.clone())),
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(120)).await;
    signal.cancel_at(started + Duration::from_millis(parent_ms));
    let result = pending.await;
    match expected {
        "success" => assert_eq!(
            json(get(get(&result, "ok"), "data")),
            r#"{"query":"q","organic":[]}"#
        ),
        "timeout" => {
            assert_eq!(text(get(get(&result, "error"), "code")), "tool_timeout");
            assert_eq!(
                text(get(get(&result, "error"), "message")),
                format!("web_search timed out after {budget}ms.")
            );
        }
        "abort" => {
            assert_eq!(
                text(get(get(&result, "error"), "code")),
                "tool_execution_failed"
            );
            assert_eq!(
                text(get(get(&result, "error"), "message")),
                "web_search (glm) request failed: Network request aborted by parent signal."
            );
        }
        _ => panic!("unknown oracle case"),
    }
    assert_eq!(*calls.lock().unwrap(), 1);

    let transport = DelayedTransport {
        delay: Duration::from_millis(response_ms),
        calls: Arc::new(Mutex::new(0)),
    };
    let request = HttpRequest {
        method: "GET",
        url: "https://example.com/".into(),
        headers: Vec::new(),
        body: None,
    };
    let signal = ToolCancellation::default();
    let ctx = context(None, empty_env(), Some(signal.clone()));
    let mut options = network_fetch_options(budget);
    options.parent = Some(&ctx);
    let started = tokio::time::Instant::now();
    let mut pending = Box::pin(network_fetch(&transport, &request, &options));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(120)).await;
    signal.cancel_at(started + Duration::from_millis(parent_ms));
    match pending.await {
        Ok(mut response) => {
            assert_eq!(expected, "success");
            assert_eq!(response.status, 200);
            assert_eq!(
                response.read_body(1024, Some(&ctx)).await.unwrap(),
                br#"{"search_result":[]}"#
            );
        }
        Err(error) => {
            let (code, message) = if expected == "timeout" {
                (
                    "network_timeout",
                    format!("Network request timed out after {budget}ms."),
                )
            } else {
                assert_eq!(expected, "abort");
                (
                    "network_abort",
                    "Network request aborted by parent signal.".into(),
                )
            };
            assert_eq!(error.code, code);
            assert_eq!(error.message.to_utf8().unwrap(), message);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn parent_event_before_response() {
    parent_event_case(100.0, 60, 30, "abort").await;
}

#[tokio::test(start_paused = true)]
async fn parent_event_equal_response() {
    parent_event_case(100.0, 60, 60, "success").await;
}

#[tokio::test(start_paused = true)]
async fn parent_event_after_response() {
    parent_event_case(100.0, 60, 110, "success").await;
}

#[tokio::test(start_paused = true)]
async fn parent_event_before_timeout() {
    parent_event_case(60.0, 100, 30, "abort").await;
}

#[tokio::test(start_paused = true)]
async fn parent_event_equal_timeout() {
    parent_event_case(60.0, 100, 60, "timeout").await;
}

#[tokio::test(start_paused = true)]
async fn parent_event_after_timeout() {
    parent_event_case(60.0, 100, 110, "timeout").await;
}

#[tokio::test(start_paused = true)]
async fn parent_event_all_equal() {
    parent_event_case(60.0, 60, 60, "timeout").await;
}

/// The exact reported sequence uses cancel(), whose event time is now=120.
#[tokio::test(start_paused = true)]
async fn parent_event_cancel_records_now_before_repoll() {
    for (budget, success) in [(100.0, true), (30.0, false)] {
        let (tool, _) = delayed_search(budget, Duration::from_millis(60));
        let signal = ToolCancellation::default();
        let mut pending = Box::pin(run(
            &tool,
            object(vec![("query", JsValue::from("q"))]),
            context(None, empty_env(), Some(signal.clone())),
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(120)).await;
        signal.cancel();
        assert_eq!(signal.cancelled_at(), Some(tokio::time::Instant::now()));
        let result = pending.await;
        if success {
            assert_eq!(
                json(get(get(&result, "ok"), "data")),
                r#"{"query":"q","organic":[]}"#
            );
        } else {
            assert_eq!(text(get(get(&result, "error"), "code")), "tool_timeout");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn parent_event_first_cancellation_is_retained() {
    let signal = ToolCancellation::default();
    let clone = signal.clone();
    let first = tokio::time::Instant::now();
    signal.cancel();
    tokio::time::advance(Duration::from_millis(120)).await;
    clone.cancel();
    clone.cancel_synchronously();
    assert_eq!(clone.cancelled_at(), Some(first));
    assert!(signal.is_cancelled());
    signal.cancelled().await;

    // A repeated synchronous call must not change an already ordered event.
    let (tool, _) = delayed_search(100.0, Duration::from_millis(60));
    let signal = ToolCancellation::default();
    let mut pending = Box::pin(run(
        &tool,
        object(vec![("query", JsValue::from("q"))]),
        context(None, empty_env(), Some(signal.clone())),
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(120)).await;
    signal.cancel();
    signal.cancel_synchronously();
    assert!(get(&pending.await, "ok").as_object().is_some());
}

struct PendingBody(Arc<AtomicBool>);
impl hyper::body::Body for PendingBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
        if self.0.load(Ordering::SeqCst) {
            std::task::Poll::Ready(None)
        } else {
            std::task::Poll::Pending
        }
    }
}

struct PendingBodyTransport(Arc<AtomicBool>);
impl ModelHttpTransport for PendingBodyTransport {
    fn send(&self, _: hyper::Request<Full<Bytes>>) -> TransportFuture {
        TransportFuture::after(
            Duration::from_millis(60),
            Ok(hyper::Response::new(PendingBody(self.0.clone()).boxed())),
        )
    }
}

/// Ready body frames belong to the earlier response's continuation, but a
/// Pending body yields to cancellation. No loopback listener is required.
#[tokio::test(start_paused = true)]
async fn parent_event_later_abort_still_cancels_pending_body() {
    let mut options = WebSearchOptions::new(Arc::new(PendingBodyTransport(Arc::new(
        AtomicBool::new(false),
    ))));
    options.provider = Some("glm".into());
    options.api_key = Some("k".into());
    options.timeout_ms = Some(100.0);
    let tool = WebSearchTool::new(options);
    let signal = ToolCancellation::default();
    let mut pending = Box::pin(run(
        &tool,
        object(vec![("query", JsValue::from("q"))]),
        context(None, empty_env(), Some(signal.clone())),
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(120)).await;
    signal.cancel();
    let result = pending.await;
    assert_eq!(
        text(get(get(&result, "error"), "code")),
        "tool_execution_failed"
    );
    assert!(
        text(get(get(&result, "error"), "message"))
            .contains("Network request aborted by parent signal.")
    );
}

#[tokio::test(start_paused = true)]
async fn parent_event_body_continuation_is_consumed_once() {
    let ready = Arc::new(AtomicBool::new(false));
    let transport = PendingBodyTransport(ready.clone());
    let request = HttpRequest {
        method: "GET",
        url: "https://example.com/".into(),
        headers: Vec::new(),
        body: None,
    };
    let signal = ToolCancellation::default();
    let ctx = context(None, empty_env(), Some(signal.clone()));
    let mut options = network_fetch_options(100.0);
    options.parent = Some(&ctx);
    let mut pending = Box::pin(network_fetch(&transport, &request, &options));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(120)).await;
    signal.cancel();
    let mut response = pending.await.unwrap();
    assert_eq!(
        response.read_body(1024, Some(&ctx)).await,
        Err(pilotdeck_gateway::runtime::tools::builtin::web::network::BodyError::Aborted)
    );
    ready.store(true, Ordering::SeqCst);
    assert_eq!(
        response.read_body(1024, Some(&ctx)).await,
        Err(pilotdeck_gateway::runtime::tools::builtin::web::network::BodyError::Aborted)
    );
}

async fn parent_event_during_overdue_retry(delayed_second: bool) {
    let calls = Arc::new(Mutex::new(0));
    let mut options = WebSearchOptions::new(Arc::new(RetryTransport {
        calls: calls.clone(),
        delayed_second,
    }));
    options.provider = Some("glm".into());
    options.api_key = Some("k".into());
    options.timeout_ms = Some(100.0);
    options.random = random();
    let tool = WebSearchTool::new(options);
    let signal = ToolCancellation::default();
    let mut pending = Box::pin(run(
        &tool,
        object(vec![("query", JsValue::from("q"))]),
        context(None, empty_env(), Some(signal.clone())),
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(120)).await;
    signal.cancel();
    let result = pending.await;
    if delayed_second {
        assert_eq!(text(get(get(&result, "error"), "code")), "tool_timeout");
    } else {
        assert_eq!(
            json(get(get(&result, "ok"), "data")),
            r#"{"query":"q","organic":[]}"#
        );
    }
    assert_eq!(*calls.lock().unwrap(), 2);
}

#[tokio::test(start_paused = true)]
async fn parent_event_after_retry_preserves_immediate_response() {
    parent_event_during_overdue_retry(false).await;
}

#[tokio::test(start_paused = true)]
async fn parent_event_after_retry_preserves_earlier_outer_timeout() {
    parent_event_during_overdue_retry(true).await;
}

/// web_fetch's 60 s hop timer must participate in the same arbitration.
#[tokio::test(start_paused = true)]
async fn web_fetch_parent_event_preserves_response_before_overdue_hop_timeout() {
    let calls = Arc::new(Mutex::new(0));
    let mut fetcher = UrlFetcher::new(Arc::new(DelayedTransport {
        delay: Duration::from_millis(60),
        calls: calls.clone(),
    }));
    fetcher.cache = Arc::new(UrlCache::default());
    let signal = ToolCancellation::default();
    let ctx = context(None, empty_env(), Some(signal.clone()));
    let mut pending =
        Box::pin(fetcher.get_url_markdown_content("https://example.com/r5-response", Some(&ctx)));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_secs(70)).await;
    signal.cancel();
    assert!(matches!(
        pending.await.unwrap(),
        pilotdeck_gateway::runtime::tools::builtin::web::fetcher::FetchResult::Fetched { .. }
    ));
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test(start_paused = true)]
async fn web_fetch_parent_event_preserves_hop_timeout_before_late_response() {
    let calls = Arc::new(Mutex::new(0));
    let mut fetcher = UrlFetcher::new(Arc::new(DelayedTransport {
        delay: Duration::from_secs(65),
        calls: calls.clone(),
    }));
    fetcher.cache = Arc::new(UrlCache::default());
    let signal = ToolCancellation::default();
    let ctx = context(None, empty_env(), Some(signal.clone()));
    let mut pending =
        Box::pin(fetcher.get_url_markdown_content("https://example.com/r5-timeout", Some(&ctx)));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_secs(70)).await;
    signal.cancel();
    assert_eq!(
        pending.await.unwrap_err(),
        pilotdeck_gateway::runtime::tools::builtin::web::fetcher::FetchError::Other(
            "Network request aborted by parent signal.".into()
        )
    );
    assert_eq!(*calls.lock().unwrap(), 1);
}

fn doc_harness() -> Harness {
    let h = Harness::new();
    h.begin(
        &object(vec![(
            "replies",
            JsValue::Array(vec![object(vec![
                ("status", JsValue::Number(200.0)),
                ("reason", JsValue::from("OK")),
                (
                    "headers",
                    object(vec![("content-type", JsValue::from("text/plain"))]),
                ),
                ("body", JsValue::from("doc body")),
            ])]),
        )]),
        None,
    );
    h
}

fn fetch_tool_with_model(h: &Harness, model: Arc<dyn SecondaryModel>) -> WebFetchTool {
    WebFetchTool::new(WebFetchToolOptions {
        model: Some(model),
        provider: None,
        model_id: None,
        max_output_tokens: None,
        fetcher: plain_fetcher(h),
    })
}

fn secondary_abort_error() -> SecondaryModelEvent {
    SecondaryModelEvent::Throw {
        is_error: true,
        value: object(vec![
            ("name", JsValue::from("AbortError")),
            ("message", JsValue::from("This operation was aborted")),
        ]),
    }
}

/// A provider that, like the real client, aborts a pending request when the
/// call's signal fires and rejects `next()` with its AbortError. The tool is
/// `for await`-shaped, so that rejection reaches the catch and is wrapped as
/// a secondary model failure (probe `fetch-cancel-order.out`,
/// `abort-throw`).
struct AbortThrowModel {
    signal_seen: Arc<Mutex<Option<bool>>>,
    started: Arc<tokio::sync::Notify>,
}
struct AbortThrowStream {
    context: ToolRunContext,
}
impl SecondaryModel for AbortThrowModel {
    fn stream(
        &self,
        _request: JsValue,
        signal: Option<ToolRunContext>,
    ) -> Box<dyn SecondaryStream> {
        *self.signal_seen.lock().unwrap() = Some(signal.is_some());
        self.started.notify_one();
        Box::new(AbortThrowStream {
            context: signal.expect("the tool passes its signal"),
        })
    }
}
impl SecondaryStream for AbortThrowStream {
    fn next(&mut self) -> SecondaryNext<'_> {
        let context = self.context.clone();
        Box::pin(async move {
            context.cancelled().await;
            Some(secondary_abort_error())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secondary_stream_cancel_maps_provider_abort_error() {
    let h = doc_harness();
    let signal_seen = Arc::new(Mutex::new(None));
    let started = Arc::new(tokio::sync::Notify::new());
    let model: Arc<dyn SecondaryModel> = Arc::new(AbortThrowModel {
        signal_seen: signal_seen.clone(),
        started: started.clone(),
    });
    let tool = fetch_tool_with_model(&h, model);
    let signal = ToolCancellation::default();
    let context = context(None, empty_env(), Some(signal.clone()));
    let cancel = signal.clone();
    let wait_started = started.clone();
    tokio::spawn(async move {
        wait_started.notified().await;
        cancel.cancel();
    });
    let input = h.real(&object(vec![
        ("url", JsValue::from("$TLS/doc")),
        ("prompt", JsValue::from("Q")),
    ]));
    let started_at = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2), run(&tool, input, context))
        .await
        .expect("a signal-aware provider must interrupt the pending next()");
    assert!(
        started_at.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started_at.elapsed()
    );
    assert_eq!(*signal_seen.lock().unwrap(), Some(true), "signal passed");
    let error = get(&result, "error");
    assert_eq!(
        text(get(error, "code")),
        "tool_execution_failed",
        "{}",
        json(&result)
    );
    assert_eq!(
        text(get(error, "message")),
        "web_fetch secondary model failed: This operation was aborted"
    );
    assert_eq!(
        json(get(error, "details")),
        r#"{"stage":"secondary_model","message":"This operation was aborted"}"#
    );
}

/// The other `for await` order: `next()` resolves with an event after the
/// signal fired. That event reaches the top-of-body `signal.aborted` check,
/// which wins over the event (probe `fetch-cancel-order.out`, `abort-event`).
struct LateEventModel {
    started: Arc<tokio::sync::Notify>,
}
struct LateEventStream {
    context: ToolRunContext,
}
impl SecondaryModel for LateEventModel {
    fn stream(
        &self,
        _request: JsValue,
        signal: Option<ToolRunContext>,
    ) -> Box<dyn SecondaryStream> {
        self.started.notify_one();
        Box::new(LateEventStream {
            context: signal.expect("the tool passes its signal"),
        })
    }
}
impl SecondaryStream for LateEventStream {
    fn next(&mut self) -> SecondaryNext<'_> {
        let context = self.context.clone();
        Box::pin(async move {
            context.cancelled().await;
            Some(SecondaryModelEvent::Event(object(vec![
                ("type", JsValue::from("text_delta")),
                ("text", JsValue::from("late")),
            ])))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secondary_stream_event_after_cancel_is_tool_aborted() {
    let h = doc_harness();
    let started = Arc::new(tokio::sync::Notify::new());
    let model: Arc<dyn SecondaryModel> = Arc::new(LateEventModel {
        started: started.clone(),
    });
    let tool = fetch_tool_with_model(&h, model);
    let signal = ToolCancellation::default();
    let context = context(None, empty_env(), Some(signal.clone()));
    let cancel = signal.clone();
    let wait_started = started.clone();
    tokio::spawn(async move {
        wait_started.notified().await;
        cancel.cancel();
    });
    let input = h.real(&object(vec![
        ("url", JsValue::from("$TLS/doc")),
        ("prompt", JsValue::from("Q")),
    ]));
    let result = tokio::time::timeout(Duration::from_secs(2), run(&tool, input, context))
        .await
        .expect("the event arrives after the signal");
    let error = get(&result, "error");
    assert_eq!(
        text(get(error, "code")),
        "tool_aborted",
        "{}",
        json(&result)
    );
    assert_eq!(
        text(get(error, "message")),
        "web_fetch aborted before completion."
    );
    assert!(matches!(get(error, "details"), JsValue::Undefined));
}

/// Third `for await` order: `next()` resolves done after the signal fired.
/// The loop exits without a body iteration, so no `signal.aborted` check runs
/// and the empty-text check reports (probe `fetch-cancel-order.out`,
/// `abort-done`).
struct AbortDoneModel {
    started: Arc<tokio::sync::Notify>,
}
struct AbortDoneStream {
    context: ToolRunContext,
}
impl SecondaryModel for AbortDoneModel {
    fn stream(
        &self,
        _request: JsValue,
        signal: Option<ToolRunContext>,
    ) -> Box<dyn SecondaryStream> {
        self.started.notify_one();
        Box::new(AbortDoneStream {
            context: signal.expect("the tool passes its signal"),
        })
    }
}
impl SecondaryStream for AbortDoneStream {
    fn next(&mut self) -> SecondaryNext<'_> {
        let context = self.context.clone();
        Box::pin(async move {
            context.cancelled().await;
            None
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secondary_stream_done_after_cancel_reaches_visible_text_check() {
    let h = doc_harness();
    let started = Arc::new(tokio::sync::Notify::new());
    let model: Arc<dyn SecondaryModel> = Arc::new(AbortDoneModel {
        started: started.clone(),
    });
    let tool = fetch_tool_with_model(&h, model);
    let signal = ToolCancellation::default();
    let context = context(None, empty_env(), Some(signal.clone()));
    let cancel = signal.clone();
    let wait_started = started.clone();
    tokio::spawn(async move {
        wait_started.notified().await;
        cancel.cancel();
    });
    let input = h.real(&object(vec![
        ("url", JsValue::from("$TLS/doc")),
        ("prompt", JsValue::from("Q")),
    ]));
    let result = tokio::time::timeout(Duration::from_secs(2), run(&tool, input, context))
        .await
        .expect("the stream ends after the signal");
    let error = get(&result, "error");
    assert_eq!(
        text(get(error, "code")),
        "tool_execution_failed",
        "{}",
        json(&result)
    );
    assert!(
        text(get(error, "message")).contains("returned no visible text"),
        "{}",
        json(&result)
    );
    let details = get(error, "details");
    assert_eq!(text(get(details, "stage")), "secondary_model");
    assert_eq!(text(get(details, "mode")), "llm");
}

/// Negative control for the event order: the same `text_delta` without a
/// cancellation is ordinary content, so `tool_aborted` above comes from the
/// signal check and not from the event.
struct ImmediateEventModel;
struct ImmediateEventStream {
    done: bool,
}
impl SecondaryModel for ImmediateEventModel {
    fn stream(
        &self,
        _request: JsValue,
        _signal: Option<ToolRunContext>,
    ) -> Box<dyn SecondaryStream> {
        Box::new(ImmediateEventStream { done: false })
    }
}
impl SecondaryStream for ImmediateEventStream {
    fn next(&mut self) -> SecondaryNext<'_> {
        if self.done {
            return Box::pin(async { None });
        }
        self.done = true;
        Box::pin(async {
            Some(SecondaryModelEvent::Event(object(vec![
                ("type", JsValue::from("text_delta")),
                ("text", JsValue::from("late")),
            ])))
        })
    }
}

#[tokio::test]
async fn negative_control_secondary_event_without_cancel_succeeds() {
    let h = doc_harness();
    let tool = fetch_tool_with_model(&h, Arc::new(ImmediateEventModel));
    let input = h.real(&object(vec![
        ("url", JsValue::from("$TLS/doc")),
        ("prompt", JsValue::from("Q")),
    ]));
    let result = run(&tool, input, context(None, empty_env(), None)).await;
    let ok = get(&result, "ok");
    assert_eq!(
        json(get(ok, "content")),
        r#"[{"type":"text","text":"late"}]"#
    );
}
