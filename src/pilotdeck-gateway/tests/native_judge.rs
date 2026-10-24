//! Real HTTP -> native provider -> registry -> token-saver decision, compared
//! with the fixed TS implementation. The independent peer records every body;
//! no fake ModelInvocationProvider or RouterModelInvocation supplies results.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::model::{
    http::HyperModelTransport,
    provider_registry::{ModelInvocationProviderRegistry, NativeModelInvocationProvider},
    router::{
        InvokeErrorKind, RouterConfig, RouterDecisionInput, RouterDeps, RouterModelInvocation,
        RouterRuntime,
    },
    stats::TokenStatsCollector,
};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing fixture field {key}"))
}
fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().expect("UTF-8")
}
fn json_value(value: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
fn cases() -> Vec<JsValue> {
    let oracle = baseline::oracle(include_str!("fixtures/native-judge/oracle.json"));
    let JsValue::Array(cases) = field(&oracle, "cases") else {
        panic!("cases array")
    };
    let ids: Vec<_> = cases.iter().map(|case| text(field(case, "id"))).collect();
    assert_eq!(
        ids,
        [
            "simple",
            "complex",
            "auth",
            "bad-request",
            "retryable",
            "missing-provider",
            "missing-model"
        ]
    );
    cases.clone()
}

/// HTTP is observed below the production transport. Reads are bounded and the
/// listener/accepted tasks belong to this peer, including panic/drop cleanup.
struct Peer {
    origin: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl Peer {
    async fn start(case: &JsValue) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let body = stringify_json(field(case, "response")).unwrap().unwrap();
        let status = field(case, "status").as_number().unwrap() as u16;
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (stop, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stopping => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.expect("HTTP peer accept");
                        let response = response.clone();
                        let recorded = recorded.clone();
                        connections.spawn(async move {
                            tokio::time::timeout(Duration::from_secs(10), exchange(stream, response, recorded))
                                .await.expect("bounded HTTP peer");
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => {
                        result.unwrap().expect("HTTP peer task");
                    }
                }
            }
            while let Some(result) = connections.join_next().await {
                result.expect("HTTP peer task");
            }
        });
        Self {
            origin,
            requests,
            stop: Some(stop),
            task: Some(task),
        }
    }
    fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
    async fn finish(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.task.take().unwrap())
            .await
            .expect("HTTP peer stopped")
            .expect("HTTP peer joined");
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
async fn exchange(mut stream: TcpStream, response: String, recorded: Arc<Mutex<Vec<Value>>>) {
    let mut bytes = Vec::new();
    let end = loop {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request headers incomplete");
        bytes.extend_from_slice(&chunk[..read]);
        assert!(bytes.len() < 1024 * 1024, "request too large");
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
    let first: Vec<_> = headers.lines().next().unwrap().split_whitespace().collect();
    let method = first[0].to_owned();
    let path = first[1].to_owned();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .expect("content-length");
    assert!(length < 1024 * 1024, "request body bound");
    while bytes.len() < end + length {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request body incomplete");
        bytes.extend_from_slice(&chunk[..read]);
    }
    let body: Value = serde_json::from_slice(&bytes[end..end + length]).unwrap();
    recorded
        .lock()
        .unwrap()
        .push(json!({ "method": method, "path": path, "body": body }));
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}

async fn run(case: &JsValue) {
    let id = text(field(case, "id"));
    let peer = Peer::start(case).await;
    let providers = Arc::new(ModelInvocationProviderRegistry::new(
        tokio::runtime::Handle::current(),
    ));
    let definitions = field(field(field(case, "snapshot"), "config"), "model");
    for (key, provider) in field(definitions, "providers")
        .as_object()
        .unwrap()
        .entries()
    {
        if id == "missing-provider" && key.equals_str("judge") {
            continue;
        }
        let mut provider = provider.as_object().unwrap().clone();
        provider.insert("url".into(), format!("{}/v1", peer.origin).as_str().into());
        if id == "missing-model" && key.equals_str("judge") {
            provider.insert("models".into(), JsValue::Object(JsObject::new()));
        }
        providers
            .register(Arc::new(NativeModelInvocationProvider::new(
                JsValue::Object(provider),
                Arc::new(HyperModelTransport::new()),
            )))
            .unwrap();
    }
    let completion = providers
        .complete(field(case, "directRequest").clone(), None)
        .await;
    let completion = match completion {
        Ok(value) => {
            json!({ "content": json_value(field(&value, "content")), "finishReason": json_value(field(&value, "finishReason")) })
        }
        Err(error) => json!({ "error": {
            "kind": match error.kind { InvokeErrorKind::Provider => "provider", InvokeErrorKind::Request => "request", InvokeErrorKind::Other => "other" },
            "code": error.code.map(|code| code.to_utf8().unwrap()),
            "message": error.message.to_utf8().unwrap(), "retryable": error.retryable,
        }}),
    };
    assert_eq!(
        completion,
        json_value(field(case, "completion")),
        "{id}: complete classification"
    );
    assert_eq!(
        peer.take(),
        json_value(field(case, "completeHttp"))
            .as_array()
            .unwrap()
            .clone(),
        "{id}: complete HTTP"
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut deps = RouterDeps::new(providers.clone());
    deps.events = Arc::new(move |event| sink.lock().unwrap().push(event));
    deps.clock = Arc::new(|| 0.0);
    deps.stats = Some(Arc::new(Mutex::new(TokenStatsCollector::new(None, None))));
    let runtime =
        RouterRuntime::with_deps(RouterConfig::from_snapshot(field(case, "snapshot")), deps)
            .unwrap();
    let input = field(case, "input");
    let decision = tokio::time::timeout(
        Duration::from_secs(15),
        runtime.decide(&RouterDecisionInput {
            request: field(input, "request").clone(),
            session_id: field(input, "sessionId").as_string().unwrap().clone(),
            is_main_agent: true,
            abort: None,
            metadata: None,
        }),
    )
    .await
    .expect("bounded native judge")
    .unwrap();
    assert_eq!(
        json_value(&decision),
        json_value(field(case, "decision")),
        "{id}: decision"
    );
    assert_eq!(
        json_value(&JsValue::Array(events.lock().unwrap().clone())),
        json_value(field(case, "events")),
        "{id}: failure code/message and attempts"
    );
    let http = peer.take();
    assert_eq!(
        http,
        json_value(field(case, "judgeHttp"))
            .as_array()
            .unwrap()
            .clone(),
        "{id}: independently observed judge requests"
    );
    for request in &http {
        assert_eq!(request["body"]["stream"], false);
        assert_eq!(request["body"]["model"], "j");
    }
    if ["auth", "bad-request"].contains(&id.as_str()) {
        assert_eq!(http.len(), 1, "non-retryable provider must not retry");
    }
    if id.starts_with("missing-") {
        assert!(
            http.is_empty(),
            "missing registration/model must not call HTTP"
        );
    }
    providers.dispose().await.unwrap();
    peer.finish().await;
    println!(
        "NATIVE_JUDGE_PASS {id}: HTTP={} decision={} events={}",
        http.len(),
        json_value(&decision),
        json_value(&JsValue::Array(events.lock().unwrap().clone()))
    );
}

#[tokio::test]
async fn real_native_complete_selects_both_token_saver_tiers_like_ts() {
    for case in cases()
        .iter()
        .filter(|case| ["simple", "complex"].contains(&text(field(case, "id")).as_str()))
    {
        run(case).await;
    }
}
#[tokio::test]
async fn real_native_provider_errors_keep_code_message_and_retryability_like_ts() {
    for case in cases().iter().filter(|case| {
        ["auth", "bad-request", "retryable"].contains(&text(field(case, "id")).as_str())
    }) {
        run(case).await;
    }
}
#[tokio::test]
async fn real_native_unconfigured_provider_and_model_do_not_issue_http_like_ts() {
    for case in cases()
        .iter()
        .filter(|case| text(field(case, "id")).starts_with("missing-"))
    {
        run(case).await;
    }
}
#[test]
#[ignore = "requires fixed PDRS_BASELINE and its Node/tsx installation"]
fn native_judge_fixture_regenerates_identically() {
    baseline::regenerates_identically("native-judge");
}
