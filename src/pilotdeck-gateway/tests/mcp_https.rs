//! HTTPS MCP replay against the real TS oracle.
//!
//! `tests/fixtures/mcp-https/generate.mts` ran the fixed baseline's
//! `McpClient` over a local TLS server (the scripted `mock_server.mjs` in
//! `--http` + `MOCK_TLS` mode, whose certificate is signed by the shared
//! throwaway CA in `tests/fixtures/model-stream/tls.json`) and recorded the
//! steps plus every request the server saw. This test starts the same mock
//! and replays the cases with `McpClient::with_tls_roots` (the shared
//! rustls connector, trusted with the fixture CA only for this test). The
//! untrusted-certificate test at the bottom uses `McpClient::new` and its
//! bundled Mozilla roots, proving the fixture CA never becomes a production
//! trust source.
//!
//! Covered: `https` initial URLs for both `streamable_http` and legacy `sse`
//! (initialize, tools/list, tools/call), `http:`→`https:` and `https:`→`http:`
//! 307/308 redirects (method/body preserved, cross-origin `authorization`
//! dropped), a 307 SSE redirect, TLS handshakes that stall (10 s connect
//! deadline shared with the hooks transport), refused TLS ports (explicit,
//! https's default 443, and an IPv6 `[::1]` peer whose message must use
//! Node's bare `::1:<port>` form), and the legacy SSE first-connect error
//! causes (self-signed certificate, connect timeout, refused) the SDK's
//! `EventSource` flattens into `SSE error: TypeError: fetch failed: …`.

#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::mcp::{
    client::{McpCallOptions, McpClient, McpClientOptions},
    protocol::{McpServerSpec, McpTransportSpec},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use scratch::Scratch;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

static UNDEFINED: JsValue = JsValue::Undefined;
const TLS: &str = include_str!("fixtures/model-stream/tls.json");
const ORACLE: &str = include_str!("fixtures/mcp-https/oracle.json");
/// The stalled handshake must fail on the 10 s connect deadline, well before
/// the case's 30 s client handshake timeout; leave room for scheduling.
const STALL_BOUND: Duration = Duration::from_secs(12);

fn oracle() -> JsValue {
    baseline::oracle(ORACLE)
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    stringify_json(value).expect("serializable").expect("json")
}
fn string(value: &JsValue, what: &str) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("{what} must be a string"))
        .to_utf8()
        .unwrap()
}
fn array<'a>(value: &'a JsValue, what: &str) -> &'a [JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("{what} must be an array"),
    }
}
fn object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}
fn map_strings(value: &JsValue, map: &dyn Fn(&str) -> String) -> JsValue {
    match value {
        JsValue::String(text) => {
            JsValue::String(map(&String::from_utf16_lossy(text.units())).into())
        }
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|v| map_strings(v, map)).collect())
        }
        JsValue::Object(entries) => {
            let mut out = JsObject::new();
            for (key, item) in entries.own_entries() {
                out.insert(key.clone(), map_strings(item, map));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

/// One per-run loopback endpoint, and the placeholder the oracle uses.
struct Host {
    origin: String,
    other: String,
    authority: String,
    placeholder: &'static str,
    /// A bracketed IPv6 authority in a message (the wrong Rust form) keeps its
    /// brackets but loses the port, so the diff still shows them.
    authority_scrub: String,
    /// Node prints an IPv6 peer without brackets; scrub only its port so the
    /// oracle keeps pinning the bare `::1` form.
    message_address: Option<String>,
    message_replacement: Option<String>,
}
fn add_host(hosts: &mut Vec<Host>, scheme: &str, host: &str, port: u16, placeholder: &'static str) {
    let bracketed = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let authority = format!("{bracketed}:{port}");
    let other = if scheme == "https" { "http" } else { "https" };
    hosts.push(Host {
        origin: format!("{scheme}://{authority}"),
        other: format!("{other}://{authority}"),
        authority,
        placeholder,
        authority_scrub: if host.contains(':') {
            format!("{bracketed}:{placeholder}")
        } else {
            placeholder.to_owned()
        },
        message_address: host.contains(':').then(|| format!("{host}:{port}")),
        message_replacement: host.contains(':').then(|| format!("{host}:{placeholder}")),
    });
}
/// The oracle's URL templates carry the scheme; only the authority is filled.
fn fill(value: &JsValue, hosts: &[Host]) -> JsValue {
    map_strings(value, &|text| {
        let mut out = text.to_owned();
        for host in hosts {
            out = out.replace(host.placeholder, &host.authority);
        }
        out
    })
}
fn scrub(value: &JsValue, hosts: &[Host]) -> JsValue {
    map_strings(value, &|text| {
        let mut out = text.to_owned();
        for host in hosts {
            out = out
                .replace(&host.origin, host.placeholder)
                .replace(&host.other, host.placeholder)
                .replace(&host.authority, &host.authority_scrub);
            if let (Some(address), Some(replacement)) =
                (&host.message_address, &host.message_replacement)
            {
                out = out.replace(address, replacement);
            }
        }
        out
    })
}
fn pairs(value: &JsValue) -> Option<Vec<(String, String)>> {
    let object = value.as_object()?;
    Some(
        object
            .own_entries()
            .into_iter()
            .map(|(key, value)| (key.to_utf8().unwrap(), string(value, "header value")))
            .collect(),
    )
}
fn spec(case: &JsValue, url: String) -> McpServerSpec {
    let transport = string(field(case, "transport"), "transport");
    McpServerSpec {
        id: "mock".into(),
        transport: match transport.as_str() {
            "streamable_http" => McpTransportSpec::StreamableHttp {
                url,
                headers: pairs(field(case, "headers")),
            },
            "sse" => McpTransportSpec::Sse {
                url,
                headers: pairs(field(case, "headers")),
            },
            other => panic!("unknown transport {other}"),
        },
        call_timeout_ms: None,
    }
}
fn options(value: &JsValue) -> McpClientOptions {
    McpClientOptions {
        call_timeout_ms: field(value, "callTimeoutMs").as_number(),
        handshake_timeout_ms: field(value, "handshakeTimeoutMs").as_number(),
        ..McpClientOptions::default()
    }
}

// ------------------------------------------------------------------ TLS

fn tls_material() -> (rustls::RootCertStore, Arc<rustls::ServerConfig>) {
    let material = parse_json(TLS).unwrap();
    let ca =
        CertificateDer::from_pem_slice(string(field(&material, "ca"), "ca").as_bytes()).unwrap();
    let cert = CertificateDer::from_pem_slice(string(field(&material, "cert"), "cert").as_bytes())
        .unwrap();
    let key =
        PrivateKeyDer::from_pem_slice(string(field(&material, "key"), "key").as_bytes()).unwrap();
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

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

// -------------------------------------------------------------- HTTP mock

struct Mock {
    _child: tokio::process::Child,
    port: u16,
}
impl Mock {
    async fn start(dir: &Path, tls: Option<&Path>) -> Self {
        use tokio::io::AsyncBufReadExt;
        let mut command = tokio::process::Command::new("node");
        command
            .arg(fixture_path("tests/fixtures/mcp-client/mock_server.mjs"))
            .arg("--http")
            .env("MOCK_DIR", dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(tls) = tls {
            command.env("MOCK_TLS", tls);
        }
        let mut child = command.spawn().expect("spawn HTTP mock");
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let line = lines
            .next_line()
            .await
            .unwrap()
            .expect("HTTP mock port line");
        let port = line
            .strip_prefix("PORT ")
            .expect("PORT line")
            .parse()
            .unwrap();
        Self {
            _child: child,
            port,
        }
    }
}
// --------------------------------------------------------- redirect server

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Reads one request, returns its path (and drains the body, so the client is
/// not reset mid-upload). `None` for a failed TLS handshake or closed socket.
fn read_path(stream: &mut impl Read) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(count) => {
                buffer.extend_from_slice(&chunk[..count]);
                if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
                    break position;
                }
            }
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let path = head.lines().next()?.split_whitespace().nth(1)?.to_owned();
    let mut length = 0usize;
    for line in head.lines() {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => body.extend_from_slice(&chunk[..count]),
        }
    }
    Some(path)
}

struct LoopServer {
    origin: String,
    stop: Arc<AtomicBool>,
}
impl Drop for LoopServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn start_redirect(
    tls: bool,
    status: u16,
    target: String,
    config: Option<Arc<rustls::ServerConfig>>,
) -> LoopServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind redirect listener");
    listener.set_nonblocking(true).unwrap();
    let scheme = if tls { "https" } else { "http" };
    let origin = format!("{scheme}://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    socket.set_write_timeout(Some(Duration::from_secs(5))).ok();
                    let target = target.clone();
                    let config = config.clone();
                    thread::spawn(move || {
                        let response = |path: &str| {
                            format!(
                                "HTTP/1.1 {status} X\r\nlocation: {target}{path}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            )
                        };
                        if let Some(config) = config {
                            let mut stream = rustls::StreamOwned::new(
                                rustls::ServerConnection::new(config).expect("TLS connection"),
                                socket,
                            );
                            if let Some(path) = read_path(&mut stream) {
                                let _ = stream.write_all(response(&path).as_bytes());
                                let _ = stream.flush();
                            }
                        } else {
                            let mut stream = socket;
                            if let Some(path) = read_path(&mut stream) {
                                let _ = stream.write_all(response(&path).as_bytes());
                                let _ = stream.flush();
                            }
                        }
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("redirect accept: {error}"),
            }
        }
    });
    LoopServer { origin, stop }
}

// ------------------------------------------------------------ stall server

struct Stall {
    origin: String,
    sockets: Arc<Mutex<Vec<TcpStream>>>,
    stop: Arc<AtomicBool>,
}
impl Drop for Stall {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.sockets.lock().unwrap().clear();
    }
}
fn start_stall() -> Stall {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind stall listener");
    listener.set_nonblocking(true).unwrap();
    let origin = format!("https://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let sockets = Arc::new(Mutex::new(Vec::new()));
    let flag = stop.clone();
    let held = sockets.clone();
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                // Accept TCP, answer nothing: the TLS ClientHello sits there.
                Ok((socket, _)) => held.lock().unwrap().push(socket),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("stall accept: {error}"),
            }
        }
    });
    Stall {
        origin,
        sockets,
        stop,
    }
}

fn closed_origin() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    format!("https://{address}")
}

/// A `::1` port that is closed again before use; the IPv6 refused case needs
/// loopback IPv6 on the host.
fn closed_port6() -> u16 {
    let listener = TcpListener::bind(("::1", 0))
        .expect("the https-sse-refused-ipv6 oracle needs a loopback ::1");
    listener.local_addr().unwrap().port()
}

// ------------------------------------------------------------------- ops

type Fields = Vec<(&'static str, JsValue)>;
fn outcome<T>(
    result: Result<T, pilotdeck_gateway::runtime::mcp::client::McpClientError>,
    ok: impl FnOnce(T) -> JsValue,
) -> Fields {
    match result {
        Ok(value) => vec![("ok", ok(value))],
        Err(error) => vec![("error", error.to_value())],
    }
}
async fn run_op(client: McpClient, op: &JsValue) -> Fields {
    if let Some(delay) = field(op, "delayMs").as_number() {
        tokio::time::sleep(Duration::from_millis(delay as u64)).await;
    }
    match string(field(op, "op"), "op").as_str() {
        "status" => vec![("ok", JsValue::String(client.status().as_str().into()))],
        "start" => {
            let mut out = outcome(client.start().await, |()| JsValue::Null);
            if out[0].0 == "ok" {
                out.push(("instructions", JsValue::String(client.instructions())));
            }
            out
        }
        "listTools" => outcome(client.list_tools().await, |tools| {
            JsValue::Array(tools.iter().map(|tool| tool.to_value()).collect())
        }),
        "callTool" => {
            let name = field(op, "name").as_string().unwrap().clone();
            let result = client
                .call_tool(
                    &name,
                    field(op, "args"),
                    McpCallOptions {
                        timeout_ms: field(op, "timeoutMs").as_number(),
                        ..McpCallOptions::default()
                    },
                )
                .await;
            outcome(result, |result| result.to_value())
        }
        "close" => {
            client.close().await;
            vec![("ok", JsValue::Null)]
        }
        other => panic!("unknown op {other}"),
    }
}

fn read_requests(dir: &Path, hosts: &[Host]) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("http.jsonl")) else {
        return Vec::new();
    };
    let mut entries: Vec<String> = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| text_of(&scrub(&parse_json(line).unwrap(), hosts)))
        .collect();
    entries.sort();
    entries
}
fn text_of(value: &JsValue) -> String {
    stringify_json(value).expect("serializable").expect("json")
}

// ------------------------------------------------------------------ replay

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replays_ts_mcp_https_over_local_tls() {
    let node = std::process::Command::new("node").arg("--version").output();
    assert!(
        node.is_ok_and(|output| output.status.success()),
        "the MCP HTTPS replay needs `node` (>= 18) on PATH to run mock_server.mjs"
    );
    let oracle = oracle();
    let cases = array(field(&oracle, "cases"), "cases");
    assert_eq!(cases.len(), 13);
    let (roots, server_config) = tls_material();
    let untrusted_tls = fixture_path("tests/fixtures/mcp-https/untrusted-tls.json");
    let trusted_tls = fixture_path("tests/fixtures/model-stream/tls.json");
    let scratch = Scratch::new("mcp-https");
    let mut steps_seen = 0usize;
    let mut requests_seen = 0usize;
    for case in cases {
        let id = string(field(case, "id"), "id");
        let dir = scratch.path().join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("script.json"),
            text(&object(vec![("http", field(case, "script").clone())])),
        )
        .unwrap();
        let template = string(field(case, "url"), "url");
        let mock_tls = field(case, "mockTls").as_bool().unwrap_or(false);
        let untrusted = field(case, "untrustedTls").as_bool().unwrap_or(false);
        let redirect = field(case, "redirect").as_object().cloned();
        let stall = field(case, "stall").as_bool().unwrap_or(false);
        let closed = field(case, "closed").as_bool().unwrap_or(false);

        let mock = if template.contains("<mock>") || redirect.is_some() {
            Some(
                Mock::start(
                    &dir,
                    mock_tls.then_some(if untrusted {
                        untrusted_tls.as_path()
                    } else {
                        trusted_tls.as_path()
                    }),
                )
                .await,
            )
        } else {
            None
        };
        let mut hosts = Vec::new();
        if let Some(mock) = &mock {
            add_host(
                &mut hosts,
                if mock_tls { "https" } else { "http" },
                "127.0.0.1",
                mock.port,
                "<mock>",
            );
        }
        let mut _redirect = None;
        if let Some(redirect) = &redirect {
            let redirect = JsValue::Object(redirect.clone());
            let target_tls = redirect_tls(field(&redirect, "mockTls"));
            let scheme = if template.starts_with("https:") {
                "https"
            } else {
                "http"
            };
            let server = start_redirect(
                scheme == "https",
                redirect_status(field(&redirect, "status")),
                format!(
                    "{}://127.0.0.1:{}",
                    if target_tls { "https" } else { "http" },
                    mock.as_ref().unwrap().port
                ),
                (scheme == "https").then(|| server_config.clone()),
            );
            add_host(
                &mut hosts,
                scheme,
                "127.0.0.1",
                port_of(&server.origin),
                "<redirect>",
            );
            _redirect = Some(server);
        }
        let mut _stall = None;
        if stall {
            let server = start_stall();
            add_host(
                &mut hosts,
                "https",
                "127.0.0.1",
                port_of(&server.origin),
                "<stall>",
            );
            _stall = Some(server);
        }
        let mut _closed = None;
        if closed {
            let origin = closed_origin();
            add_host(
                &mut hosts,
                "https",
                "127.0.0.1",
                port_of(&origin),
                "<closed>",
            );
            _closed = Some(origin);
        }
        if field(case, "closed6").as_bool().unwrap_or(false) {
            add_host(&mut hosts, "https", "::1", closed_port6(), "<closed6>");
        }

        let url = string(
            &fill(&JsValue::String(template.clone().into()), &hosts),
            "url",
        );
        let client = McpClient::new(spec(case, url), options(field(case, "options")))
            .with_tls_roots(roots.clone());
        let ops = array(field(case, "ops"), "ops");
        let expected = array(field(case, "steps"), "steps");
        assert_eq!(ops.len(), expected.len(), "{id}: step count");
        for (index, op) in ops.iter().enumerate() {
            let started = Instant::now();
            let stalling = (id == "https-handshake-stall" || id == "https-sse-handshake-stall")
                && string(field(op, "op"), "op") == "start";
            let fields = if stalling {
                match tokio::time::timeout(STALL_BOUND, run_op(client.clone(), op)).await {
                    Ok(fields) => fields,
                    Err(_) => panic!(
                        "{id}: connect deadline did not fire within {STALL_BOUND:?} ({:?})",
                        started.elapsed()
                    ),
                }
            } else {
                run_op(client.clone(), op).await
            };
            if stalling {
                assert!(
                    started.elapsed() >= Duration::from_secs(9),
                    "{id}: returned after {:?}, before the 10 s deadline",
                    started.elapsed()
                );
            }
            let mut step: Fields = vec![("step", op.clone())];
            step.extend(fields);
            step.push(("status", JsValue::String(client.status().as_str().into())));
            let actual = scrub(&object(step), &hosts);
            assert_eq!(
                text(&actual),
                text(&expected[index]),
                "{id}: step {index} ({})",
                string(field(op, "op"), "op")
            );
            steps_seen += 1;
        }
        // Detached replies/cancellations may still be in flight.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let actual = read_requests(&dir, &hosts);
        let expected: Vec<String> = array(field(case, "requests"), "requests")
            .iter()
            .map(text_of)
            .collect();
        assert_eq!(actual, expected, "{id}: server request log");
        requests_seen += actual.len();
        drop(mock);
    }
    assert_eq!(steps_seen, 45);
    assert_eq!(requests_seen, 30);
}

fn redirect_tls(value: &JsValue) -> bool {
    value.as_bool().unwrap_or(false)
}
fn redirect_status(value: &JsValue) -> u16 {
    value.as_number().expect("redirect status") as u16
}
fn port_of(origin: &str) -> u16 {
    origin
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .expect("origin port")
}

/// The fixture CA is trusted only through `with_tls_roots`; the production
/// constructor must reject it with its bundled Mozilla roots.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_roots_do_not_trust_the_fixture_ca() {
    let scratch = Scratch::new("mcp-https-untrusted");
    std::fs::write(
        scratch.path().join("script.json"),
        r#"{"http":{"initialize":[{"json":{"jsonrpc":"2.0","id":"$id","result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"m","version":"1"}}}}]}}"#,
    )
    .unwrap();
    let mock = Mock::start(
        scratch.path(),
        Some(&fixture_path("tests/fixtures/model-stream/tls.json")),
    )
    .await;
    let client = McpClient::new(
        McpServerSpec {
            id: "mock".into(),
            transport: McpTransportSpec::StreamableHttp {
                url: format!("https://127.0.0.1:{}/mcp", mock.port),
                headers: None,
            },
            call_timeout_ms: None,
        },
        McpClientOptions::default(),
    );
    let error = client.start().await.unwrap_err();
    assert_eq!(
        error.client_code(),
        Some(pilotdeck_gateway::runtime::mcp::client::McpClientErrorCode::HandshakeFailed),
        "{}",
        error.message()
    );
}

/// Re-runs the real TS generator and requires a byte-identical oracle.
#[test]
#[ignore]
fn mcp_https_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("mcp-https");
}
