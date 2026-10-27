//! #867 actual loopback replays of the Node v22.22.0 corpus.
use super::*;
use crate::runtime::memory::edgeclaw::llm::transport::{LlmMemoryExtractor, StructuredInput};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

fn corpus() -> Value {
    let raw = include_str!("../../../../tests/fixtures/model-http-encoding/oracle.json");
    crate::test_baseline::oracle(raw);
    serde_json::from_str(raw).expect("#867 corpus JSON")
}
fn text(v: &Value) -> &str {
    v.as_str().expect("#867 corpus string")
}
fn unhex(v: &Value) -> Vec<u8> {
    text(v)
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("#867 hex UTF-8"), 16)
                .expect("#867 hex byte")
        })
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).expect("#867 test config JSON")
}
fn transport() -> HyperModelTransport {
    HyperModelTransport::from_config(webpki_client_config(), None)
}
async fn bounded<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("#867 loopback watchdog")
}
struct Server {
    url: String,
    release: oneshot::Sender<bool>,
    task: JoinHandle<Vec<u8>>,
}
async fn server(case: &Value, streaming: bool) -> Server {
    server_with_disconnect_hook(case, streaming, None).await
}
type DisconnectHook = oneshot::Receiver<(oneshot::Sender<bool>, JoinHandle<Vec<u8>>)>;
async fn server_with_disconnect_hook(
    case: &Value,
    streaming: bool,
    on_request: Option<DisconnectHook>,
) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#867 loopback bind");
    let url = format!(
        "http://{}/",
        listener.local_addr().expect("#867 listener address")
    );
    let case = case.clone();
    let (release, released) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = bounded(listener.accept()).await.expect("#867 accept");
        let mut request = Vec::new();
        let header_end;
        loop {
            let mut chunk = [0; 1024];
            let size = bounded(socket.read(&mut chunk))
                .await
                .expect("#867 request read");
            assert!(size > 0, "#867 request not truncated");
            request.extend_from_slice(&chunk[..size]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = end + 4;
                break;
            }
        }
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, v) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().expect("#867 content length"))
            })
            .unwrap_or(0);
        while request.len() < header_end + length {
            let mut chunk = [0; 1024];
            let size = bounded(socket.read(&mut chunk))
                .await
                .expect("#867 request payload read");
            assert!(size > 0, "#867 request payload not truncated");
            request.extend_from_slice(&chunk[..size]);
        }
        if let Some(on_request) = on_request {
            let (release, predecessor) = bounded(on_request)
                .await
                .expect("#867 redirect disconnect hook installed");
            release
                .send(true)
                .expect("#867 redirect disconnect trigger");
            bounded(predecessor)
                .await
                .expect("#867 redirect predecessor closed before target response");
        }
        let (wire, tail) = if streaming {
            (unhex(&case["prefix"]), unhex(&case["tail"]))
        } else {
            (unhex(&case["wire"]), Vec::new())
        };
        let status = case["status"].as_u64().unwrap_or(200);
        let framing = if streaming {
            "Transfer-Encoding: chunked\r\n".to_owned()
        } else {
            format!("Content-Length: {}\r\n", wire.len())
        };
        let encodings = case["encoding"]
            .as_array()
            .map(|values| values.iter().map(text).collect::<Vec<_>>())
            .unwrap_or_else(|| vec![text(&case["encoding"])]);
        let encoding_headers = encodings
            .iter()
            .map(|value| format!("Content-Encoding: {value}\r\n"))
            .collect::<String>();
        let location = case
            .get("location")
            .map(|value| format!("Location: {}\r\n", text(value)))
            .unwrap_or_default();
        let head = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\n{encoding_headers}{location}{framing}Connection: keep-alive\r\n\r\n"
        );
        bounded(socket.write_all(head.as_bytes()))
            .await
            .expect("#867 response headers write");
        if streaming {
            let first = format!("{:x}\r\n", wire.len());
            bounded(socket.write_all(first.as_bytes()))
                .await
                .expect("#867 first chunk size");
            bounded(socket.write_all(&wire))
                .await
                .expect("#867 first chunk bytes");
            bounded(socket.write_all(b"\r\n"))
                .await
                .expect("#867 first chunk end");
            if bounded(released).await.expect("#867 release event") {
                if case["finish"] == "disconnect" {
                    return request[..header_end].to_vec();
                }
                if !tail.is_empty() {
                    let size = format!("{:x}\r\n", tail.len());
                    bounded(socket.write_all(size.as_bytes()))
                        .await
                        .expect("#867 tail size");
                    bounded(socket.write_all(&tail))
                        .await
                        .expect("#867 tail bytes");
                    bounded(socket.write_all(b"\r\n"))
                        .await
                        .expect("#867 tail delimiter");
                }
                bounded(socket.write_all(b"0\r\n\r\n"))
                    .await
                    .expect("#867 tail end");
            } else {
                let mut byte = [0];
                let closed = bounded(socket.read(&mut byte)).await;
                assert!(
                    matches!(closed, Ok(0)) || closed.is_err(),
                    "#867 dropping decoder closes predecessor"
                );
            }
        } else {
            bounded(socket.write_all(&wire))
                .await
                .expect("#867 response bytes write");
        }
        request[..header_end].to_vec()
    });
    Server { url, release, task }
}
async fn response(url: &str, method: &str) -> Response<ModelBody> {
    response_result(url, method)
        .await
        .expect("#867 HTTP response")
}
async fn response_result(url: &str, method: &str) -> TransportResult {
    response_result_with_follow_mode(url, method, false).await
}
async fn response_result_with_follow_mode(
    url: &str,
    method: &str,
    follow_redirects: bool,
) -> TransportResult {
    let mut request = Request::builder()
        .method(method)
        .uri(url)
        .body(Full::new(Bytes::new()))
        .expect("#867 wire request");
    if follow_redirects {
        request
            .extensions_mut()
            .insert(crate::fetch_redirect::FollowRedirects);
    }
    bounded(transport().send(request)).await
}

#[tokio::test]
async fn encoding_layer_limit_matches_node() {
    for case in corpus()["layers"].as_array().expect("#867 layer cases") {
        let peer = server(case, false).await;
        let result = response_result(&peer.url, text(&case["method"])).await;
        if case["result"].get("error").is_some() {
            let error = result.expect_err("#867 encoding layer cap rejects before body decoding");
            assert_eq!(
                error.message,
                text(&case["result"]["error"]),
                "#867 encoding layer cap fetch error"
            );
            assert_eq!(
                error.cause_code, None,
                "#867 encoding layer cap has no cause code"
            );
            assert_eq!(
                case["result"]["name"], "TypeError",
                "#867 encoding layer cap Node error name"
            );
        } else {
            let response = result.expect("#867 encoding layer cap accepts five and HEAD");
            let body = bounded(response.into_body().collect())
                .await
                .expect("#867 encoding layer cap accepted body")
                .to_bytes();
            assert_eq!(
                &body[..],
                text(&case["result"]["text"]).as_bytes(),
                "#867 encoding layer cap accepted bytes"
            );
        }
        bounded(peer.task).await.expect("#867 layer server task");
    }
}

#[tokio::test]
async fn encoding_token_boundaries_match_node() {
    for case in corpus()["tokenBoundaries"]
        .as_array()
        .expect("#867 token cases")
    {
        let peer = server(case, false).await;
        let result = response_result(&peer.url, "GET").await;
        if let Some(error) = case["result"].get("error") {
            let failure = result.expect_err("#867 token boundary rejects excess codings");
            assert_eq!(
                failure.message,
                text(error),
                "#867 token boundary fetch failure"
            );
            assert_eq!(failure.cause_code, None, "#867 token boundary cause code");
        } else {
            let response = result.expect("#867 token boundary accepts response");
            assert_eq!(
                u64::from(response.status().as_u16()),
                case["result"]["status"]
                    .as_u64()
                    .expect("#867 token status"),
                "#867 token boundary status"
            );
            let body = bounded(response.into_body().collect())
                .await
                .expect("#867 token boundary body");
            assert_eq!(
                hex(&body.to_bytes()),
                text(&case["result"]["hex"]),
                "#867 token boundary bytes: {}",
                case["name"]
            );
        }
        bounded(peer.task).await.expect("#867 token server task");
    }
}

#[tokio::test]
async fn encoding_redirects_match_node_follow() {
    for case in corpus()["redirects"]
        .as_array()
        .expect("#867 redirect cases")
    {
        // Provider fetches use shared follow; web URL fetching calls the
        // transport directly with manual redirects. Exercise both modes.
        let follows = case["redirect"] == "follow"
            && case["location"].as_str().is_some_and(|v| !v.is_empty())
            && case["status"] != 200;
        let disconnect = case["bodyState"] == "disconnect";
        let (install_hook, hook) = oneshot::channel();
        let target = if follows {
            Some(server_with_disconnect_hook(&json!({
                "encoding": if case["targetSix"] == true {"gzip,gzip,gzip,gzip,gzip,gzip"} else {"identity"},
                "wire": hex(b"target-ok"),
            }), false, disconnect.then_some(hook)).await)
        } else {
            None
        };
        let mut initial = case.clone();
        if let Some(target) = &target {
            initial["location"] = json!(target.url);
        }
        let streaming = case.get("bodyState").is_some();
        initial["prefix"] = json!("78");
        initial["tail"] = json!("");
        initial["finish"] = json!("disconnect");
        let peer = server(&initial, streaming).await;
        let (release, peer_task) = if disconnect {
            install_hook
                .send((peer.release, peer.task))
                .expect("#867 redirect install disconnect hook");
            (None, None)
        } else {
            (Some(peer.release), Some(peer.task))
        };
        let transport = transport();
        let result = if follows {
            bounded(crate::fetch_redirect::follow(
                url::Url::parse(&peer.url).expect("#867 redirect URL"),
                hyper::Method::GET,
                hyper::HeaderMap::new(),
                Bytes::new(),
                || TransportFailure::fetch_failed(None),
                |request| transport.send(request),
            ))
            .await
        } else {
            // Observe terminal rejection at the transport boundary too: an
            // erroneously followed empty Location would otherwise fail later
            // with the same outer error after issuing an extra request.
            response_result_with_follow_mode(&peer.url, "GET", case["redirect"] == "follow").await
        };
        if let Some(error) = case["result"].get("error") {
            let failure = result.expect_err("#867 redirect checks terminal codings");
            assert_eq!(failure.message, text(error), "#867 redirect fetch failure");
        } else {
            let response = result.expect("#867 redirect skips intermediate codings");
            assert_eq!(
                u64::from(response.status().as_u16()),
                case["result"]["status"]
                    .as_u64()
                    .expect("#867 redirect status"),
                "#867 redirect target status"
            );
            let body = bounded(response.into_body().collect())
                .await
                .expect("#867 redirect target body");
            assert_eq!(
                &body.to_bytes()[..],
                text(&case["result"]["text"]).as_bytes(),
                "#867 redirect target bytes"
            );
        }
        if streaming && !disconnect {
            release
                .expect("#867 redirect pending predecessor handle")
                .send(false)
                .expect("#867 redirect predecessor release");
        }
        if let Some(task) = peer_task {
            bounded(task).await.expect("#867 redirect server task");
        }
        if let Some(target) = target {
            bounded(target.task)
                .await
                .expect("#867 redirect target task");
        }
    }
}

fn size_body(case: &Value) -> ModelBody {
    // Split compressed input too: the decoder must refill across wire chunks.
    let wire = unhex(&case["wire"]);
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    for bytes in wire.chunks(7) {
        sender
            .send(hyper::body::Frame::data(Bytes::copy_from_slice(bytes)))
            .expect("#867 size wire frame send");
    }
    drop(sender);
    let response = Response::builder()
        .header("content-encoding", text(&case["encoding"]))
        .body(ControlledBody(receiver).boxed())
        .expect("#867 size response");
    encoding::decode_response(&hyper::Method::GET, response, false)
        .expect("#867 size encoding accepted")
        .into_body()
}

#[tokio::test]
async fn decoded_size_boundary_rejects_before_text_allocation() {
    let oracle = corpus();
    for case in oracle["sizes"].as_array().expect("#867 size cases") {
        let limit = case["limit"].as_u64().expect("#867 size limit") as usize;
        let size = case["size"].as_u64().expect("#867 decoded size") as usize;
        let _limit = LimitGuard::set(limit);
        let mut capped = CappedText::new();
        let mut body = size_body(case);
        let mut decoded = 0;
        while let Some(frame) = bounded(body.frame()).await {
            let frame = frame.expect("#867 size decoded frame");
            if let Some(bytes) = frame.data_ref() {
                assert!(
                    bytes.iter().all(|b| *b == 97),
                    "#867 size decoded bytes match Node"
                );
                decoded += bytes.len();
                let before = capped.converted_bytes;
                let accepted = capped.push(bytes);
                assert_eq!(
                    accepted,
                    decoded <= limit,
                    "#867 decoded size cap rejects excess bytes"
                );
                if decoded > limit {
                    assert_eq!(
                        capped.converted_bytes, before,
                        "#867 decoded size cap runs before text allocation"
                    );
                    assert_eq!(
                        capped.text.capacity(),
                        0,
                        "#867 decoded size cap releases text buffer"
                    );
                }
            }
        }
        assert_eq!(
            decoded, size,
            "#867 size fixture traverses all decoded bytes"
        );
        assert_eq!(
            capped.converted_bytes, limit,
            "#867 decoded size cap converts only bounded bytes"
        );
        let mut body = size_body(case);
        let result = bounded(response_text(&mut body, &StreamOptions::default(), false)).await;
        if size == limit {
            let expected = "a".repeat(limit);
            assert_eq!(
                capped.finish().expect("#867 exact decoded size accepted"),
                expected,
                "#867 exact decoded size bytes"
            );
            assert_eq!(
                result.expect("#867 exact decoded response accepted"),
                expected,
                "#867 exact decoded response bytes"
            );
        } else {
            assert!(
                capped.finish().is_err(),
                "#867 decoded size cap rejects excess text"
            );
            let error = result.expect_err("#867 decoded size cap rejects excess response");
            let node = oracle["sizeProbe"]
                .as_array()
                .expect("#867 real Node size probe")
                .iter()
                .find(|probe| probe["encoding"] == case["encoding"] && probe.get("error").is_some())
                .expect("#867 real Node oversize error");
            assert!(
                error.name().equals_str(text(&node["name"])),
                "#867 decoded size cap Node error name"
            );
            assert!(
                error.message().equals_str(text(&node["error"])),
                "#867 decoded size cap Node error message"
            );
        }
    }
}

#[tokio::test]
async fn decoded_size_boundary_shared_text_reader() {
    for case in corpus()["sizes"]
        .as_array()
        .expect("#867 shared size cases")
    {
        let limit = case["limit"].as_u64().expect("#867 shared size limit") as usize;
        let size = case["size"].as_u64().expect("#867 shared decoded size") as usize;
        let result = bounded(crate::fetch_redirect::read_text_body(
            size_body(case),
            limit,
            None,
        ))
        .await;
        if size == limit {
            assert_eq!(
                result.expect("#867 shared exact decoded size accepted"),
                vec![97; limit],
                "#867 shared exact decoded bytes"
            );
        } else {
            assert_eq!(
                result,
                Err(crate::fetch_redirect::BodyError::TooLong),
                "#867 shared decoded size cap rejects excess bytes"
            );
        }
    }
}

#[tokio::test]
async fn decoded_wire_matches_node() {
    for case in corpus()["cases"].as_array().expect("#867 cases") {
        let peer = server(case, false).await;
        let response = response(&peer.url, text(&case["method"])).await;
        let expected = &case["result"];
        if expected.get("hex").is_some() {
            assert_eq!(
                response.headers()["content-encoding"].as_bytes(),
                text(&expected["encoding"]).as_bytes(),
                "#867 wire headers preserved"
            );
            assert_eq!(
                response.headers()["content-length"].as_bytes(),
                text(&expected["length"]).as_bytes(),
                "#867 wire length preserved"
            );
        }
        let result = bounded(response.into_body().collect()).await;
        if let Some(error) = expected.get("error") {
            assert_eq!(
                result.err().map(|e| e.to_string()).as_deref(),
                Some(text(error)),
                "#867 raw response error matches Node: {}",
                case["name"]
            );
        } else {
            assert!(
                result.is_ok(),
                "#867 raw response matches Node: {} {result:?}",
                case["name"]
            );
            assert_eq!(
                hex(&result.expect("#867 raw response matches Node").to_bytes()),
                text(&expected["hex"]),
                "#867 raw response matches Node: {}",
                case["name"]
            );
        }
        bounded(peer.task).await.expect("#867 server task");
    }
}

#[test]
fn header_byte_string_matches_node() {
    for case in corpus()["headers"].as_array().expect("#867 header cases") {
        let units = case["units"]
            .as_array()
            .expect("#867 header units")
            .iter()
            .map(|u| u.as_u64().expect("#867 header unit") as u16)
            .collect();
        let key_units = case["keyUnits"]
            .as_array()
            .expect("#867 header key units")
            .iter()
            .map(|u| u.as_u64().expect("#867 header key unit") as u16)
            .collect();
        let mut headers = vec![(
            JsString::from_utf16(key_units),
            JsValue::String(JsString::from_utf16(units)),
        )];
        if case["name"] == "conversion-order" {
            headers.insert(0, ("bad name".into(), "ok".into()));
        }

        if case["name"] == "duplicate" {
            headers.push(("x-test".into(), "ÿ".into()));
        }
        let result = build_http_request("http://127.0.0.1/", &headers);
        if let Some(expected) = case["result"].get("hex") {
            let (_, map) = result.expect("#867 ByteString accepted");
            assert_eq!(
                hex(map.get("x-test").expect("#867 header exists").as_bytes()),
                text(expected),
                "#867 header bytes match Node: {}",
                case["name"]
            );
        } else {
            assert_eq!(
                result
                    .err()
                    .map(|e| e.message.to_utf8().expect("#867 header error UTF-8")),
                Some(text(&case["result"]["error"]).to_owned()),
                "#867 header errors match Node: {}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn both_provider_entries_decode_node_responses() {
    let oracle = corpus();
    for case in oracle["cases"]
        .as_array()
        .expect("#867 provider cases")
        .iter()
        .take(4)
    {
        for edgeclaw in [false, true] {
            let peer = server(case, false).await;
            let provider_headers = json!({"X-Test":"é","x-test":"ÿ"});
            let actual = if edgeclaw {
                let config = js(
                    json!({"agents":{"defaults":{"model":{"primary":"p/m"}}},"models":{"providers":{"p":{"api":"chat","baseUrl":peer.url,"apiKey":"k","headers":provider_headers}}}}),
                );
                let extractor =
                    LlmMemoryExtractor::new(config, Arc::new(transport()), Arc::new(()));
                bounded(extractor.call_structured_json(&StructuredInput {
                    system_prompt: "s".into(),
                    user_prompt: "u".into(),
                    agent_id: None,
                    request_label: "#867".into(),
                    timeout_ms: Some(5000.0),
                }))
                .await
                .expect("#867 both entries decode response")
                .to_utf8()
                .expect("#867 EdgeClaw result UTF-8")
            } else {
                let config = js(
                    json!({"providers":{"p":{"id":"p","protocol":"openai-completions","url":peer.url,"apiKey":"k","headers":provider_headers,"retry":{"maxRetries":0},"models":{"m":{"id":"m","capabilities":{"supportsStreaming":true,"maxOutputTokens":8},"multimodal":{"input":["text"]}}}}}}),
                );
                let request = js(json!({"provider":"p","model":"m","messages":[]}));
                let value = bounded(complete_model(
                    &transport(),
                    &request,
                    &config,
                    &StreamOptions::default(),
                ))
                .await
                .expect("#867 both entries decode response");
                value
                    .get("content")
                    .and_then(|v| {
                        if let JsValue::Array(a) = v {
                            a.first()
                        } else {
                            None
                        }
                    })
                    .and_then(|v| v.get("text"))
                    .and_then(JsValue::as_string)
                    .expect("#867 both entries decode response")
                    .to_utf8()
                    .expect("#867 model result UTF-8")
            };
            let expected: Value =
                serde_json::from_str(text(&oracle["payload"])).expect("#867 payload JSON");
            assert_eq!(
                actual,
                text(&expected["choices"][0]["message"]["content"]),
                "#867 both entries decode response: {} edge={edgeclaw}",
                case["name"]
            );
            let wire = bounded(peer.task).await.expect("#867 provider server task");
            let expected_header = b"x-test: \xe9, \xff\r\n";
            assert!(
                wire.windows(expected_header.len())
                    .any(|w| w == expected_header),
                "#867 both entries send Latin-1 header bytes"
            );
        }
    }
}

#[tokio::test]
async fn compressed_body_yields_and_cancels() {
    for case in corpus()["lifecycle"]
        .as_array()
        .expect("#867 lifecycle cases")
    {
        let peer = server(case, true).await;
        let mut body = response(&peer.url, "GET").await.into_body();
        let first = bounded(data_frame(&mut body))
            .await
            .expect("#867 streaming first frame")
            .expect("#867 streaming first frame data");
        assert_eq!(
            &first[..],
            text(&case["result"]["first"]).as_bytes(),
            "#867 decoded prefix before EOF"
        );
        if case["cancel"].as_bool().expect("#867 cancel flag") {
            drop(body);
            peer.release.send(false).expect("#867 cancel predecessor");
        } else {
            peer.release.send(true).expect("#867 release predecessor");
            let rest = bounded(body.collect())
                .await
                .expect("#867 streamed tail")
                .to_bytes();
            assert_eq!(
                &rest[..],
                text(&case["result"]["rest"]).as_bytes(),
                "#867 decoded tail after release"
            );
        }
        bounded(peer.task)
            .await
            .expect("#867 lifecycle server task");
    }
}

#[test]
#[ignore = "requires fixed TS checkout and Node v22.22.0"]
fn encoding_fixture_regenerates_identically() {
    crate::test_baseline::regenerates_identically("model-http-encoding");
}

#[tokio::test]
async fn compression_eof_waits_for_http_eof() {
    for case in corpus()["endings"].as_array().expect("#867 ending cases") {
        let peer = server(case, true).await;
        let mut body = response(&peer.url, "GET").await.into_body();
        let first = bounded(data_frame(&mut body))
            .await
            .expect("#867 EOF first frame")
            .expect("#867 EOF frame data");
        assert_eq!(
            &first[..],
            text(&case["result"]["first"]).as_bytes(),
            "#867 EOF decoded first"
        );
        let mut next = Box::pin(data_frame(&mut body));
        std::future::poll_fn(|cx| {
            assert!(
                next.as_mut().poll(cx).is_pending(),
                "#867 compression EOF waits for HTTP EOF"
            );
            Poll::Ready(())
        })
        .await;
        if case["finish"] == "cancel" {
            drop(next);
            drop(body);
            peer.release.send(false).expect("#867 EOF cancel signal");
        } else {
            peer.release.send(true).expect("#867 EOF release signal");
            let result = bounded(next).await;
            if case["finish"] == "disconnect" {
                assert!(
                    matches!(result, Some(Err(BodyFailure::Terminated))),
                    "#867 wire error after compression EOF: {result:?}"
                );
            } else {
                assert!(
                    result.is_none(),
                    "#867 HTTP EOF completes decoder: {result:?}"
                );
            }
        }
        bounded(peer.task).await.expect("#867 EOF server task");
    }
}

struct ControlledBody(tokio::sync::mpsc::UnboundedReceiver<hyper::body::Frame<Bytes>>);
impl hyper::body::Body for ControlledBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
        self.0.poll_recv(cx).map(|frame| frame.map(Ok))
    }
}
#[tokio::test(start_paused = true)]
async fn compressed_wire_progress_preserves_both_idle_clocks() {
    for application_idle in [false, true] {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let source = Response::builder()
            .header("content-encoding", "gzip")
            .body(ControlledBody(receiver).boxed())
            .expect("#867 controlled response");
        let mut body = encoding::decode_response(&hyper::Method::GET, source, false)
            .expect("#867 controlled encoding accepted")
            .into_body();
        let wire = unhex(&corpus()["endings"][0]["prefix"]);
        let options = StreamOptions {
            body_timeout_ms: 1000.0,
            ..StreamOptions::default()
        };
        let mut continuation = false;
        let future = async {
            if application_idle {
                read_frame(&mut body, 900.0, &options, &mut continuation)
                    .await
                    .map(|v| {
                        v.map(|b| String::from_utf8_lossy(&b).into_owned())
                            .unwrap_or_default()
                    })
            } else {
                response_text(&mut body, &options, false).await
            }
        };
        let mut future = Box::pin(future);
        let header_bytes = if application_idle { 3 } else { 4 };
        for i in 0..header_bytes {
            sender
                .send(hyper::body::Frame::data(Bytes::copy_from_slice(
                    &wire[i..i + 1],
                )))
                .expect("#867 controlled header send");
            std::future::poll_fn(|cx| {
                assert!(
                    future.as_mut().poll(cx).is_pending(),
                    "#867 wire activity keeps body idle alive"
                );
                Poll::Ready(())
            })
            .await;
            tokio::time::advance(Duration::from_millis(400)).await;
        }
        if application_idle {
            let result = std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
                Poll::Ready(result) => Poll::Ready(result),
                Poll::Pending => panic!("#867 wire activity does not reset application idle"),
            })
            .await;
            let error = result.expect_err("#867 wire activity does not reset application idle");
            assert!(
                error.name().equals_str("StreamIdleTimeoutError"),
                "#867 wire activity does not reset application idle: {error:?}"
            );
        } else {
            std::future::poll_fn(|cx| {
                assert!(
                    future.as_mut().poll(cx).is_pending(),
                    "#867 wire activity keeps body idle alive"
                );
                Poll::Ready(())
            })
            .await;
            sender
                .send(hyper::body::Frame::data(Bytes::copy_from_slice(
                    &wire[header_bytes..],
                )))
                .expect("#867 controlled body send");
            drop(sender);
            assert_eq!(
                future
                    .await
                    .expect("#867 wire activity keeps body idle alive"),
                text(&corpus()["progress"][0]["result"]),
                "#867 wire progress matches Node"
            );
        }
    }
}

async fn data_frame(body: &mut ModelBody) -> Option<Result<Bytes, BodyFailure>> {
    while let Some(frame) = body.frame().await {
        match frame {
            Err(error) => return Some(Err(error)),
            Ok(frame) => {
                if let Ok(bytes) = frame.into_data()
                    && !bytes.is_empty()
                {
                    return Some(Ok(bytes));
                }
            }
        }
    }
    None
}

#[tokio::test]
async fn interrupted_compressed_wire_is_not_a_truncated_representation() {
    for original in corpus()["interrupted"]
        .as_array()
        .expect("#867 interrupted cases")
    {
        let mut case = original.clone();
        case["tail"] = json!("");
        case["finish"] = json!("disconnect");
        let peer = server(&case, true).await;
        let body = response(&peer.url, "GET").await.into_body();
        peer.release.send(true).expect("#867 interrupted release");
        let error = bounded(body.collect()).await.err().map(|e| e.to_string());
        assert_eq!(
            error.as_deref(),
            Some(text(&case["result"]["error"])),
            "#867 incomplete HTTP body stays an error"
        );
        bounded(peer.task)
            .await
            .expect("#867 interrupted server task");
    }
}
