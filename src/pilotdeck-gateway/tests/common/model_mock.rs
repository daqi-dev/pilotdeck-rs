//! Scripted loopback provider mock (HTTP and rustls HTTPS) and the shared
//! oracle replay for `generate.mts`-style model-stream fixtures
//! (`tests/fixtures/model-stream*/`).
#![allow(dead_code)]
use pilotdeck_gateway::runtime::model::http::{
    HyperModelTransport, ModelHttpTransport, ModelStreamError, StreamOptions, StreamOutput,
    stream_model,
};
use pilotdeck_gateway::runtime::model::router::AbortFlag;
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

/// Throwaway test CA/leaf shared by every TLS smoke (see model-stream README).
pub const TLS: &str = include_str!("../fixtures/model-stream/tls.json");

pub fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}
pub fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().unwrap()
}
pub fn get<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
pub fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => &[],
    }
}

#[derive(Default)]
pub struct Exchange {
    pub replies: Vec<JsValue>,
    pub requests: Vec<JsObject>,
    /// `(placeholder, origin)` substituted into reply header values
    /// (redirect `Location`s naming `$MOCK` / `$OTHER`).
    pub materialize: Vec<(String, String)>,
}

pub struct Mock {
    pub origin: String,
    stop: Arc<AtomicBool>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub fn start_mock(
    exchange: Arc<Mutex<Exchange>>,
    excluded: Vec<String>,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Mock {
    start_tagged_mock(exchange, excluded, tls, None)
}

/// Like [`start_mock`]; every request record also carries `server: tag`
/// (the generator's second origin, `$OTHER`).
pub fn start_tagged_mock(
    exchange: Arc<Mutex<Exchange>>,
    excluded: Vec<String>,
    tls: Option<Arc<rustls::ServerConfig>>,
    tag: Option<&'static str>,
) -> Mock {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let scheme = if tls.is_some() { "https" } else { "http" };
    let flag = stop.clone();
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_nodelay(true).unwrap();
                    let exchange = exchange.clone();
                    let excluded = excluded.clone();
                    let tls = tls.clone();
                    thread::spawn(move || match tls {
                        Some(config) => {
                            let connection = rustls::ServerConnection::new(config).unwrap();
                            serve(
                                rustls::StreamOwned::new(connection, socket),
                                &exchange,
                                &excluded,
                                tag,
                            );
                        }
                        None => serve(socket, &exchange, &excluded, tag),
                    });
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("accept: {error}"),
            }
        }
    });
    Mock {
        origin: format!("{scheme}://127.0.0.1:{port}"),
        stop,
    }
}

trait Socket: Read + Write {
    fn tcp(&self) -> &TcpStream;
}
impl Socket for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}
impl Socket for rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    fn tcp(&self) -> &TcpStream {
        &self.sock
    }
}

/// method, path, lower-cased headers (duplicates joined), body.
type Received = (String, String, Vec<(String, String)>, Vec<u8>);

fn read_request(socket: &mut impl Socket) -> Option<Received> {
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
        let name = name.to_ascii_lowercase();
        let value = value.trim().to_string();
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

fn serve(
    mut socket: impl Socket,
    exchange: &Mutex<Exchange>,
    excluded: &[String],
    tag: Option<&'static str>,
) {
    while let Some((method, path, headers, body)) = read_request(&mut socket) {
        let mut kept: Vec<(String, String)> = headers
            .into_iter()
            .filter(|(name, _)| !excluded.contains(name))
            .collect();
        kept.sort();
        let mut record = JsObject::new();
        record.insert("method".into(), method.as_str().into());
        record.insert("path".into(), path.as_str().into());
        record.insert(
            "headers".into(),
            JsValue::Array(
                kept.iter()
                    .map(|(n, v)| JsValue::Array(vec![n.as_str().into(), v.as_str().into()]))
                    .collect(),
            ),
        );
        record.insert(
            "body".into(),
            String::from_utf8(body).unwrap().as_str().into(),
        );
        if let Some(tag) = tag {
            record.insert("server".into(), tag.into());
        }
        let (index, reply, materialize) = {
            let mut exchange = exchange.lock().unwrap();
            exchange.requests.push(record);
            let index = exchange.requests.len() - 1;
            (
                index,
                exchange.replies.get(index).cloned(),
                exchange.materialize.clone(),
            )
        };
        let Some(reply) = reply else {
            return; // unexpected extra request; the exact count assertion fails
        };
        let status = get(&reply, "status").as_number().unwrap_or(200.0);
        let mut head = format!("HTTP/1.1 {status} Mock\r\n");
        match get(&reply, "headers").as_object() {
            Some(headers) => {
                for (name, value) in headers.entries() {
                    let value = materialize
                        .iter()
                        .fold(text(value), |acc, (from, to)| acc.replace(from, to));
                    head.push_str(&format!("{}: {value}\r\n", name.to_utf8().unwrap()));
                }
            }
            None => head.push_str("content-type: text/event-stream\r\n"),
        }
        // Null-body statuses carry no framing at all (keep-alive stays valid).
        let no_body = status == 204.0 || status == 205.0;
        head.push_str(if no_body {
            "\r\n"
        } else {
            "transfer-encoding: chunked\r\n\r\n"
        });
        if socket
            .write_all(head.as_bytes())
            .and_then(|()| socket.flush())
            .is_err()
        {
            return;
        }
        for chunk in items(get(&reply, "chunks")) {
            let bytes = match get(chunk, "hex").as_string() {
                Some(hex) => {
                    let hex = hex.to_utf8().unwrap();
                    (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                        .collect()
                }
                None => text(get(chunk, "text")).into_bytes(),
            };
            let mut frame = format!("{:x}\r\n", bytes.len()).into_bytes();
            frame.extend(bytes);
            frame.extend(b"\r\n");
            if socket
                .write_all(&frame)
                .and_then(|()| socket.flush())
                .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_millis(4));
        }
        match get(&reply, "end")
            .as_string()
            .map(|s| s.to_utf8().unwrap())
            .as_deref()
        {
            Some("destroy") => {
                thread::sleep(Duration::from_millis(20));
                let _ = socket.tcp().shutdown(std::net::Shutdown::Both);
                return;
            }
            Some("hold") => {
                socket
                    .tcp()
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut probe = [0u8; 64];
                let closed = loop {
                    match socket.read(&mut probe) {
                        Ok(0) => break true,
                        Ok(_) => continue,
                        Err(e)
                            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                        {
                            break false;
                        }
                        Err(_) => break true,
                    }
                };
                exchange.lock().unwrap().requests[index]
                    .insert("clientClosed".into(), JsValue::Bool(closed));
                return;
            }
            _ if no_body => {}
            _ => {
                if socket
                    .write_all(b"0\r\n\r\n")
                    .and_then(|()| socket.flush())
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

pub fn merge(base: &JsValue, overrides: &JsValue) -> JsValue {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Some(overrides) = overrides.as_object() {
        for (key, value) in overrides.entries() {
            out.insert(key.clone(), value.clone());
        }
    }
    JsValue::Object(out)
}

pub fn thrown(error: &ModelStreamError) -> JsValue {
    let mut record = JsObject::new();
    record.insert("name".into(), JsValue::String(error.name()));
    record.insert("message".into(), JsValue::String(error.message()));
    match error {
        ModelStreamError::Provider(canonical) => {
            record.insert("error".into(), JsValue::Object(canonical.clone()));
        }
        ModelStreamError::Request { code, .. } => {
            record.insert("code".into(), JsValue::String(code.clone()));
        }
        ModelStreamError::Thrown { .. } => {}
    }
    JsValue::Object(record)
}
pub fn entry(key: &str, value: JsValue) -> String {
    let mut record = JsObject::new();
    record.insert(key.into(), value);
    json(&JsValue::Object(record))
}

pub fn tls_material() -> (rustls::RootCertStore, Arc<rustls::ServerConfig>) {
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

/// Replay every case of a `generate.mts` oracle against one HTTP and one
/// HTTPS mock; `cases` / `requests` are the exact expected counts.
pub async fn replay(oracle: &JsValue, cases_expected: usize, requests_expected: usize) {
    let oracle = oracle.clone();
    assert_eq!(text(get(&oracle, "node")), "v22.22.0");
    // Older corpora (model-stream-openai) do not record the undici version.
    if !get(&oracle, "undici").is_undefined() {
        assert_eq!(text(get(&oracle, "undici")), "8.3.0");
    }
    assert_eq!(text(get(&oracle, "locale")), "en-US");
    let random = get(&oracle, "random").as_number().unwrap();
    let excluded: Vec<String> = items(get(&oracle, "transportHeaders"))
        .iter()
        .map(text)
        .collect();
    let (roots, server_config) = tls_material();
    let plain_transport: Arc<dyn ModelHttpTransport> = Arc::new(HyperModelTransport::new());
    let trusted_transport: Arc<dyn ModelHttpTransport> =
        Arc::new(HyperModelTransport::with_roots(roots));

    let exchange = Arc::new(Mutex::new(Exchange::default()));
    let http = start_mock(exchange.clone(), excluded.clone(), None);
    let other = start_tagged_mock(exchange.clone(), excluded.clone(), None, Some("$OTHER"));
    let https = start_mock(exchange.clone(), excluded, Some(server_config));
    let closed = {
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        format!("http://127.0.0.1:{}", probe.local_addr().unwrap().port())
    };

    let cases = items(get(&oracle, "cases"));
    assert_eq!(cases.len(), cases_expected);
    let mut request_count = 0;
    let mut names = std::collections::HashSet::new();
    for case in cases {
        let name = text(get(case, "name"));
        assert!(names.insert(name.clone()), "duplicate case {name}");
        let target = text(get(case, "target"));
        let (origin, placeholder) = match target.as_str() {
            "mock" => (http.origin.clone(), "$MOCK"),
            "closed" => (closed.clone(), "$CLOSED"),
            _ => (https.origin.clone(), "$TLS"),
        };
        let transport = if target == "tls-trusted" {
            &trusted_transport
        } else {
            &plain_transport
        };
        let replies = items(get(case, "replies")).to_vec();
        {
            let mut exchange = exchange.lock().unwrap();
            exchange.replies = replies.clone();
            exchange.requests.clear();
            exchange.materialize = vec![
                ("$MOCK".into(), http.origin.clone()),
                ("$OTHER".into(), other.origin.clone()),
            ];
        }
        let base_provider = get(&oracle, "provider");
        let mut provider = merge(base_provider, get(case, "provider"));
        let retry = merge(
            get(base_provider, "retry"),
            get(get(case, "provider"), "retry"),
        );
        let url = match get(case, "url") {
            JsValue::String(url) => url.to_utf8().unwrap(),
            _ => text(get(base_provider, "url")),
        };
        if let JsValue::Object(object) = &mut provider {
            object.insert("retry".into(), retry);
            object.insert("url".into(), url.replace("$MOCK", &origin).as_str().into());
        }
        let mut providers = JsObject::new();
        providers.insert(
            get(base_provider, "id").as_string().unwrap().clone(),
            provider,
        );
        let mut config = JsObject::new();
        config.insert("providers".into(), JsValue::Object(providers));
        let config = JsValue::Object(config);
        let request = merge(get(&oracle, "request"), get(case, "request"));

        let abort = AbortFlag::new();
        let abort_after = get(case, "abortAfterTextDeltas").as_number();
        let options = StreamOptions {
            abort: Some(abort.clone()),
            stream_timeout_ms: get(case, "streamTimeoutMs").as_number(),
            body_timeout_ms: get(case, "bodyTimeoutMs")
                .as_number()
                .unwrap_or(StreamOptions::default().body_timeout_ms),
            random: Arc::new(move || random),
            ..StreamOptions::default()
        };
        let mut timeline = Vec::new();
        let mut text_deltas = 0.0;
        let result = {
            let mut sink = |output: StreamOutput| match output {
                StreamOutput::Event(event) => {
                    let is_text = get(&event, "type")
                        .as_string()
                        .is_some_and(|t| t.equals_str("text_delta"));
                    timeline.push(entry("event", event));
                    if is_text {
                        text_deltas += 1.0;
                        if Some(text_deltas) == abort_after {
                            abort.abort();
                        }
                    }
                }
                StreamOutput::RetryProgress(progress) => timeline.push(entry("progress", progress)),
            };
            stream_model(transport.as_ref(), &request, &config, &options, &mut sink).await
        };
        if let Err(error) = &result {
            timeline.push(entry("thrown", thrown(error)));
        }
        let holds = replies.iter().any(|r| {
            get(r, "end")
                .as_string()
                .is_some_and(|e| e.equals_str("hold"))
        });
        for _ in 0..400 {
            let pending = exchange
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|r| !r.contains_key("clientClosed"));
            if !holds || !pending {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let scrub = |s: String| {
            s.replace(&origin, placeholder)
                .replace(&other.origin, "$OTHER")
        };
        let expected: Vec<String> = items(get(case, "timeline")).iter().map(json).collect();
        let actual: Vec<String> = timeline.into_iter().map(scrub).collect();
        assert_eq!(
            actual.len(),
            expected.len(),
            "{name}: timeline length\n{actual:#?}"
        );
        for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(actual, expected, "{name}: timeline[{index}]");
        }
        let requests: Vec<String> = exchange
            .lock()
            .unwrap()
            .requests
            .iter()
            .map(|r| scrub(json(&JsValue::Object(r.clone()))))
            .collect();
        let expected: Vec<String> = items(get(case, "requests")).iter().map(json).collect();
        assert_eq!(
            requests.len(),
            replies.len(),
            "{name}: every reply consumed once"
        );
        assert_eq!(requests, expected, "{name}: request records");
        request_count += requests.len();
    }
    assert_eq!(request_count, requests_expected);
}
