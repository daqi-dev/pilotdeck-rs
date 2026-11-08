use super::*;
use crate::module_host::approvals::native_http::JsonFuture;
use pilotdeck_protocol::parse_json;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Mock {
    row: JsValue,
    fixtures: JsValue,
    calls: Mutex<Vec<JsValue>>,
    health: AtomicUsize,
}
impl JsonTransport for Mock {
    fn request(&self, input: JsonRequest) -> JsonFuture<'_> {
        assert!(
            !input.redirect_error,
            "portable owner keeps fetch redirect semantics"
        );
        let mut call = JsObject::new();
        call.insert("url".into(), input.url.as_str().into());
        call.insert("method".into(), input.method.into());
        if let Some(body) = input.body {
            call.insert("body".into(), body.as_str().into());
        }
        self.calls.lock().unwrap().push(JsValue::Object(call));
        let options = field(&self.row, "options");
        if let Some(error) = options.get("fetchError").and_then(JsValue::as_string) {
            let error = JsonFailure::Transport(error.to_utf8().unwrap());
            return Box::pin(async move { Err(error) });
        }
        let health = input.method == "GET";
        let body = if health {
            if options.get("retryManifest") == Some(&JsValue::Bool(true))
                && self.health.fetch_add(1, Ordering::SeqCst) == 0
            {
                JsValue::Object(JsObject::new())
            } else {
                options
                    .get("manifest")
                    .cloned()
                    .unwrap_or_else(|| field(&self.fixtures, "manifest").clone())
            }
        } else if let Some(envelope) = options.get("envelope") {
            envelope.clone()
        } else if options.get("invalidJson") == Some(&JsValue::Bool(true)) {
            JsValue::Undefined
        } else {
            let operation = field(&self.row, "operation").as_string().unwrap();
            let payload = if operation.equals_str("prepare") {
                crate::metadata::object([
                    ("state", field(&self.fixtures, "state").clone()),
                    (
                        "step",
                        options
                            .get("step")
                            .cloned()
                            .unwrap_or_else(|| field(&self.fixtures, "step").clone()),
                    ),
                ])
            } else {
                crate::metadata::object([
                    ("state", field(&self.fixtures, "state").clone()),
                    (
                        "result",
                        options
                            .get("result")
                            .cloned()
                            .unwrap_or_else(|| field(&self.fixtures, "result").clone()),
                    ),
                ])
            };
            let context = field(options, "context");
            let request_id = context.get("requestId").cloned().unwrap_or_else(|| {
                format!("sop.{}", operation.to_utf8().unwrap())
                    .as_str()
                    .into()
            });
            crate::metadata::object([
                ("protocolVersion", "2.0".into()),
                ("requestId", request_id),
                ("ok", JsValue::Bool(true)),
                ("outcome", "completed".into()),
                ("payload", payload),
            ])
        };
        let status = options
            .get(if health { "healthStatus" } else { "status" })
            .and_then(JsValue::as_number)
            .unwrap_or(200.0) as u16;
        Box::pin(async move {
            Ok(JsonResponse {
                status,
                body,
                json_error: None,
            })
        })
    }
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("fixture array")
    };
    items
}
fn same(actual: &JsValue, expected: &JsValue, label: &str) {
    assert_eq!(
        stringify_json(actual).unwrap(),
        stringify_json(expected).unwrap(),
        "{label}"
    );
}
#[tokio::test]
async fn actual_portable_owner_client_matches_thirty_real_ts_cases_thirty_two_outcomes() {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/native-sop/client-oracle.json"
    ))
    .unwrap();
    assert_eq!(
        field(&oracle, "baseline"),
        &JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85")
    );
    assert_eq!(field(&oracle, "forbidden"), &JsValue::Number(0.0));
    let rows = array(field(&oracle, "rows"));
    assert_eq!(rows.len(), 30);
    let mut count = 0;
    for row in rows {
        let options = field(row, "options");
        let input = field(row, "input");
        let name = field(row, "name").as_string().unwrap().to_utf8().unwrap();
        let mock = Arc::new(Mock {
            row: row.clone(),
            fixtures: field(&oracle, "fixtures").clone(),
            calls: Mutex::new(vec![]),
            health: AtomicUsize::new(0),
        });
        let client = SopClient::new(
            "http://127.0.0.1:1/base///".into(),
            options
                .get("manifestPath")
                .and_then(JsValue::as_string)
                .map(|s| s.to_utf8().unwrap()),
            options.get("expected").cloned(),
            None,
            mock.clone(),
        );
        for expected in array(field(row, "outcomes")) {
            count += 1;
            let signal = Cancellation::new();
            let result = if field(row, "operation")
                .as_string()
                .unwrap()
                .equals_str("prepare")
            {
                client
                    .prepare(
                        field(input, "bundle").clone(),
                        field(input, "state").clone(),
                        input.get("context"),
                        &signal,
                    )
                    .await
            } else {
                let tools = array(field(input, "successfulToolNames"))
                    .iter()
                    .map(|v| v.as_string().unwrap().clone())
                    .collect();
                client
                    .submit(
                        field(input, "bundle").clone(),
                        field(input, "state").clone(),
                        field(input, "proposal").clone(),
                        tools,
                        input.get("context"),
                        &signal,
                    )
                    .await
            };
            let mut outcome = JsObject::new();
            match result {
                Ok(value) => {
                    outcome.insert("ok".into(), JsValue::Bool(true));
                    outcome.insert("result".into(), value);
                }
                Err(error) => {
                    outcome.insert("ok".into(), JsValue::Bool(false));
                    outcome.insert("name".into(), "StaffDeckSopClientError".into());
                    outcome.insert("code".into(), JsValue::String(error.code));
                    outcome.insert("message".into(), JsValue::String(error.message));
                    if let Some(details) = error.details {
                        outcome.insert("details".into(), *details);
                    }
                    if let Some(retry) = error.retryability {
                        outcome.insert("retryability".into(), JsValue::String(retry));
                    }
                }
            }
            same(
                &JsValue::Object(outcome),
                expected,
                &format!("{name} outcome {count}"),
            );
        }
        let actual = mock.calls.lock().unwrap();
        let expected = array(field(row, "requests"));
        assert_eq!(actual.len(), expected.len(), "{name} request count");
        for (actual, expected) in actual.iter().zip(expected) {
            let mut request = JsObject::new();
            for key in ["url", "method", "body"] {
                if let Some(value) = expected.get(key) {
                    request.insert(key.into(), value.clone());
                }
            }
            same(
                actual,
                &JsValue::Object(request),
                &format!("{name} complete request"),
            );
        }
    }
    assert_eq!(count, 32);
}
struct Pending {
    entered: tokio::sync::Notify,
}
impl JsonTransport for Pending {
    fn request(&self, _: JsonRequest) -> JsonFuture<'_> {
        self.entered.notify_one();
        Box::pin(std::future::pending())
    }
}
#[tokio::test]
async fn cancellation_also_bounds_a_waiter_on_an_inflight_manifest() {
    let pending = Arc::new(Pending {
        entered: tokio::sync::Notify::new(),
    });
    let client = Arc::new(SopClient::new(
        "http://127.0.0.1:1".into(),
        None,
        None,
        None,
        pending.clone(),
    ));
    let first_signal = Cancellation::new();
    let second_signal = Cancellation::new();
    let first = {
        let client = client.clone();
        let signal = first_signal.clone();
        tokio::spawn(async move {
            client
                .prepare(JsValue::Null, JsValue::Null, None, &signal)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(1), pending.entered.notified())
        .await
        .expect("first request actually holds manifest lock");
    // A second call need not acquire the first caller's manifest mutex in order
    // to observe its own cancellation. Both owned tasks are joined below.
    let second = {
        let client = client.clone();
        let signal = second_signal.clone();
        tokio::spawn(async move {
            client
                .prepare(JsValue::Null, JsValue::Null, None, &signal)
                .await
        })
    };
    second_signal.abort();
    let answer = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .expect("manifest waiter cancellation is bounded")
        .unwrap()
        .unwrap_err();
    assert!(answer.code.equals_str("SOP_RUNTIME_CANCELLED"));
    assert_eq!(answer.retryability, Some("unsafe".into()));
    first_signal.abort();
    let _ = tokio::time::timeout(Duration::from_secs(1), first)
        .await
        .unwrap()
        .unwrap();
}

async fn raw_server(response: Vec<u8>, hold: bool) -> (url::Url, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let _ = socket.read(&mut request).await.unwrap();
        socket.write_all(&response).await.unwrap();
        if hold {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
    (
        url::Url::parse(&format!("http://{address}/fixture")).unwrap(),
        task,
    )
}
fn get(url: url::Url, timeout: Duration) -> JsonRequest {
    JsonRequest {
        method: "GET",
        url,
        bearer: None,
        body: None,
        parse_error_body: true,
        redirect_error: false,
        timeout,
    }
}
#[tokio::test]
async fn real_http_uses_shared_gzip_decoder_and_bounds_the_decoded_body() {
    use crate::module_host::approvals::native_http::HyperJsonTransport;
    // Self-owned gzip encoding of {}, no downloaded binary/test resource.
    let gzip = [
        31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 171, 174, 5, 0, 67, 191, 166, 163, 2, 0, 0, 0,
    ];
    let mut wire=format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",gzip.len()).into_bytes();
    wire.extend(gzip);
    let (url, server) = raw_server(wire, false).await;
    let answer = HyperJsonTransport::default()
        .request(get(url, Duration::from_secs(2)))
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(answer.body, JsValue::Object(JsObject::new()));
    let (url, server) = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\n{\"x\":1}".to_vec(),
        false,
    )
    .await;
    let answer = HyperJsonTransport::with_limit(4)
        .request(get(url, Duration::from_secs(2)))
        .await;
    server.await.unwrap();
    assert!(
        matches!(answer, Err(JsonFailure::Body(_))),
        "body refusal is not safe fetch retry permission"
    );
}
#[tokio::test]
async fn body_deadline_keeps_post_header_failure_distinct_from_connect_failure() {
    use crate::module_host::approvals::native_http::HyperJsonTransport;
    let (url, server) = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n{".to_vec(),
        true,
    )
    .await;
    let http = HyperJsonTransport::default();
    let answer = tokio::time::timeout(
        Duration::from_secs(2),
        http.request(get(url, Duration::from_millis(50))),
    )
    .await
    .expect("absolute response deadline");
    server.abort();
    let _ = server.await;
    assert!(matches!(answer, Err(JsonFailure::Body(_))));
}
