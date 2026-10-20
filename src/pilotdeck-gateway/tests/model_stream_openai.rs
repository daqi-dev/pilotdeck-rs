//! OpenAI chat-completions replay against the fixed TypeScript baseline
//! (`tests/fixtures/model-stream-openai/generate.mts`). The real TS
//! `streamModel` and this Rust port talk to the same scripted loopback mock;
//! request bytes and the event/retry/throw timeline are compared in order,
//! byte for byte. No external traffic, no TLS (the shared transport is covered
//! by `model_stream.rs`).
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::runtime::model::http::{
    HyperModelTransport, ModelHttpTransport, ModelStreamError, StreamOptions, StreamOutput,
    stream_model,
};
use pilotdeck_gateway::runtime::model::router::AbortFlag;
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
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

const ORACLE: &str = include_str!("fixtures/model-stream-openai/oracle.json");

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

#[derive(Default)]
struct Exchange {
    replies: Vec<JsValue>,
    requests: Vec<JsObject>,
}

struct Mock {
    origin: String,
    stop: Arc<AtomicBool>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn start_mock(exchange: Arc<Mutex<Exchange>>, excluded: Vec<String>) -> Mock {
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
                    let exchange = exchange.clone();
                    let excluded = excluded.clone();
                    thread::spawn(move || serve(socket, &exchange, &excluded));
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("accept: {error}"),
            }
        }
    });
    Mock {
        origin: format!("http://127.0.0.1:{port}"),
        stop,
    }
}

/// method, path, lower-cased headers (duplicates joined), body.
type Received = (String, String, Vec<(String, String)>, Vec<u8>);

fn read_request(socket: &mut TcpStream) -> Option<Received> {
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

fn serve(mut socket: TcpStream, exchange: &Mutex<Exchange>, excluded: &[String]) {
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
        let (index, reply) = {
            let mut exchange = exchange.lock().unwrap();
            exchange.requests.push(record);
            let index = exchange.requests.len() - 1;
            (index, exchange.replies.get(index).cloned())
        };
        let Some(reply) = reply else {
            return; // unexpected extra request; the exact count assertion fails
        };
        let status = get(&reply, "status").as_number().unwrap_or(200.0);
        let mut head = format!("HTTP/1.1 {status} Mock\r\n");
        match get(&reply, "headers").as_object() {
            Some(headers) => {
                for (name, value) in headers.entries() {
                    head.push_str(&format!("{}: {}\r\n", name.to_utf8().unwrap(), text(value)));
                }
            }
            None => head.push_str("content-type: text/event-stream\r\n"),
        }
        head.push_str("transfer-encoding: chunked\r\n\r\n");
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
                let _ = socket.shutdown(std::net::Shutdown::Both);
                return;
            }
            Some("hold") => {
                socket
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

fn merge(base: &JsValue, overrides: &JsValue) -> JsValue {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Some(overrides) = overrides.as_object() {
        for (key, value) in overrides.entries() {
            out.insert(key.clone(), value.clone());
        }
    }
    JsValue::Object(out)
}

fn thrown(error: &ModelStreamError) -> JsValue {
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
fn entry(key: &str, value: JsValue) -> String {
    let mut record = JsObject::new();
    record.insert(key.into(), value);
    json(&JsValue::Object(record))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ts_openai_stream_model_over_loopback_http() {
    let oracle = baseline::oracle(ORACLE);
    assert_eq!(text(get(&oracle, "node")), "v22.22.0");
    assert_eq!(text(get(&oracle, "locale")), "en-US");
    let random = get(&oracle, "random").as_number().unwrap();
    let excluded: Vec<String> = items(get(&oracle, "transportHeaders"))
        .iter()
        .map(text)
        .collect();
    let transport: Arc<dyn ModelHttpTransport> = Arc::new(HyperModelTransport::new());

    let exchange = Arc::new(Mutex::new(Exchange::default()));
    let http = start_mock(exchange.clone(), excluded);
    let closed = {
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        format!("http://127.0.0.1:{}", probe.local_addr().unwrap().port())
    };

    let cases = items(get(&oracle, "cases"));
    assert_eq!(cases.len(), 23);
    let mut request_count = 0;
    let mut names = std::collections::HashSet::new();
    for case in cases {
        let name = text(get(case, "name"));
        assert!(names.insert(name.clone()), "duplicate case {name}");
        let target = text(get(case, "target"));
        let (origin, placeholder) = match target.as_str() {
            "mock" => (http.origin.clone(), "$MOCK"),
            "closed" => (closed.clone(), "$CLOSED"),
            _ => panic!("{name}: unknown target {target}"),
        };
        let replies = items(get(case, "replies")).to_vec();
        {
            let mut exchange = exchange.lock().unwrap();
            exchange.replies = replies.clone();
            exchange.requests.clear();
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
        let scrub = |s: String| s.replace(&origin, placeholder);
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
    assert_eq!(request_count, 28);
}
