//! HTTPS HTTP-hook replay against the real TS oracle.
//!
//! `tests/fixtures/hooks-https/generate.mts` ran the baseline `HttpHookExecutor`
//! against a local TLS server whose certificate is signed by the shared
//! throwaway CA in `tests/fixtures/model-stream/tls.json`. This test starts an
//! equivalent rustls server, trusts that CA through `HttpHookExecutor::with_roots`
//! (the model transport's test pattern), and compares every result byte for
//! byte. The untrusted-certificate case uses `HttpHookExecutor::new()` and its
//! bundled Mozilla roots, so the fixture CA must not be trusted there.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::hooks::exec::{HttpHookExecutor, HttpHookOptions};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

static UNDEFINED: JsValue = JsValue::Undefined;
const TLS: &str = include_str!("fixtures/model-stream/tls.json");

fn oracle() -> JsValue {
    baseline::oracle(include_str!("fixtures/hooks-https/oracle.json"))
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().expect("utf8")
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("expected array: {value:?}"),
    }
}
fn object(value: &JsValue) -> JsObject {
    value.as_object().expect("object").clone()
}
fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().expect("json")
}

// ---------------------------------------------------------------- TLS fixture

fn tls_material() -> (rustls::RootCertStore, Arc<rustls::ServerConfig>) {
    let material = parse_json(TLS).unwrap();
    let ca = CertificateDer::from_pem_slice(text(field(&material, "ca")).as_bytes()).unwrap();
    let cert = CertificateDer::from_pem_slice(text(field(&material, "cert")).as_bytes()).unwrap();
    let key = PrivateKeyDer::from_pem_slice(text(field(&material, "key")).as_bytes()).unwrap();
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

#[derive(Default)]
struct Request {
    method: String,
    path: String,
    content_type: String,
    body: String,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn read_request(stream: &mut impl Read) -> Request {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let count = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Request::default(),
            Ok(count) => count,
        };
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            break position;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_owned();
    let path = first.next().unwrap_or("/").to_owned();
    let mut content_type = String::new();
    let mut length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-type") {
            content_type = value.trim().to_owned();
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let count = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        body.extend_from_slice(&chunk[..count]);
    }
    body.truncate(length.min(body.len()));
    Request {
        method,
        path,
        content_type,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn echo_body(request: &Request) -> String {
    format!(
        "{{\"method\":{},\"body\":{},\"contentType\":{}}}",
        serde_json::to_string(&request.method).unwrap(),
        serde_json::to_string(&request.body).unwrap(),
        serde_json::to_string(&request.content_type).unwrap(),
    )
}

fn response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

struct TlsServer {
    origin: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
}
impl Drop for TlsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn start_tls(config: Arc<rustls::ServerConfig>) -> TlsServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind TLS listener");
    listener.set_nonblocking(true).unwrap();
    let origin = format!("https://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let flag = stop.clone();
    let recorded = requests.clone();
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_nodelay(true).unwrap();
                    let config = config.clone();
                    let recorded = recorded.clone();
                    thread::spawn(move || {
                        socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
                        socket.set_write_timeout(Some(Duration::from_secs(10))).ok();
                        let mut stream = rustls::StreamOwned::new(
                            rustls::ServerConnection::new(config).expect("TLS connection"),
                            socket,
                        );
                        let request = read_request(&mut stream);
                        // A failed client handshake (untrusted-certificate case)
                        // reaches the accept loop but never sends a request.
                        if request.method.is_empty() {
                            return;
                        }
                        let body = echo_body(&request);
                        recorded.lock().unwrap().push(request);
                        let _ = stream.write_all(response(&body).as_bytes());
                        let _ = stream.flush();
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("TLS accept: {error}"),
            }
        }
    });
    TlsServer {
        origin,
        requests,
        stop,
    }
}

struct RedirectServer {
    origin: String,
    stop: Arc<AtomicBool>,
}
impl Drop for RedirectServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn start_redirect(tls_origin: String) -> RedirectServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind redirect listener");
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_nodelay(true).unwrap();
                    let tls_origin = tls_origin.clone();
                    thread::spawn(move || {
                        socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
                        let request = read_request(&mut socket);
                        let location = format!("{tls_origin}{}", request.path);
                        let head = format!(
                            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                        let _ = socket.write_all(head.as_bytes());
                        let _ = socket.flush();
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("redirect accept: {error}"),
            }
        }
    });
    RedirectServer { origin, stop }
}

/// A loopback port that is closed before the request: TLS connect is refused.
fn closed_origin() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    format!("https://{address}")
}

// ---------------------------------------------------------------- replay

#[tokio::test]
async fn http_hooks_over_tls_match_real_ts() {
    let oracle = oracle();
    let (trusted_roots, server_config) = tls_material();
    let tls = start_tls(server_config);
    let redirect = start_redirect(tls.origin.clone());
    let closed = closed_origin();
    let hook_input = object(field(&oracle, "hookInput"));
    let cases = array(field(&oracle, "cases"));
    assert!(!cases.is_empty(), "oracle has no cases");
    for case in cases {
        let name = text(field(case, "name"));
        let target = text(field(case, "url"))
            .replace("<tls>", &tls.origin)
            .replace("<http>", &redirect.origin)
            .replace("<closed>", &closed);
        let trusted = field(case, "trusted").as_bool().unwrap_or(false);
        let executor = if trusted {
            HttpHookExecutor::with_roots(trusted_roots.clone())
        } else {
            HttpHookExecutor::new()
        };
        let result = executor
            .execute(HttpHookOptions {
                url: target.into(),
                headers: None,
                allowed_env_vars: None,
                hook_input: hook_input.clone(),
                env: Some(JsObject::new()),
                signal: None,
            })
            .await;
        assert_eq!(
            json(&result.to_value()),
            text(field(case, "expected")),
            "https case {name}"
        );
    }
    // The trusted cases must have carried real requests over TLS: a direct
    // POST with the hook input body, then the 302's GET without a body.
    let requests = tls.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "TLS server request count");
    assert_eq!(requests[0].path, "/hook");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].content_type, "application/json");
    assert_eq!(requests[0].body, json(&JsValue::Object(hook_input)));
    assert_eq!(requests[1].path, "/hook");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].content_type, "");
    assert_eq!(requests[1].body, "");
}

/// Re-runs the real TS generator and requires a byte-identical oracle.
#[test]
#[ignore]
fn hooks_https_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("hooks-https");
}
