//! Token counting through the environment's outgoing HTTP proxy.
//!
//! `HyperCountTransport` builds its connector with
//! `ProxyConfig::from_settings(None)` (count_http.rs), the same env-driven
//! source as the model and hook transports. Each scenario runs in a fresh
//! child process (edition 2024 forbids `unsafe set_var`), with the proxy
//! environment set exactly for the transport, against two real pieces: the
//! scripted CONNECT proxy from tests/fixtures/mcp-proxy/connect_proxy.mjs
//! and an in-test hyper target. The scenarios pin the wiring the review
//! flagged as untested (R6): proxy taken from `HTTP_PROXY`, the CONNECT
//! tunnel carrying the real POST body and auth headers, proxy Basic auth
//! from the proxy URL, a 407 refusal surfacing as `fetch failed` with no
//! target traffic, `NO_PROXY` bypassing the proxy, and a redirect hop
//! re-evaluating the bypass per hop.
//!
//! The target is addressed as `0.0.0.0` because the env source always adds
//! `127.0.0.1` and `localhost` to `NO_PROXY`; `0.0.0.0` reaches the same
//! local server on this host without taking the implicit bypass. A TLS
//! target through the tunnel is not repeated here: the CONNECT machinery is
//! shared with the MCP transport and covered by tests/mcp_proxy.rs.

#[path = "common/proxy_env.rs"]
mod proxy_env;
#[path = "common/scratch.rs"]
mod scratch;

use http_body_util::BodyExt;
use pilotdeck_gateway::runtime::model::{
    CountHttpRequest, HyperCountTransport, ProviderCountTransport,
};
use pilotdeck_protocol::{JsObject, parse_json, stringify_json};
use proxy_env::PROXY_ENV_KEYS;
use scratch::Scratch;
use std::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncBufReadExt;

/// The scripted CONNECT proxy (same fixture as the MCP proxy replay).
struct ProxyChild(tokio::process::Child);
impl ProxyChild {
    async fn start(dir: &Path, auth: bool, status: Option<u16>) -> (Self, u16, std::path::PathBuf) {
        let log = dir.join("proxy.jsonl");
        let mut command = tokio::process::Command::new("node");
        command
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/mcp-proxy/connect_proxy.mjs"),
            )
            .env("PROXY_LOG", &log)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        if auth {
            command.env("PROXY_AUTH", "user:pass");
        }
        if let Some(status) = status {
            command.env("PROXY_STATUS", status.to_string());
        }
        let mut child = command.spawn().expect("spawn node CONNECT proxy");
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let line = lines.next_line().await.unwrap().expect("PORT line");
        let port = line
            .strip_prefix("PORT ")
            .expect("PORT line")
            .parse()
            .unwrap();
        (Self(child), port, log)
    }
}
impl Drop for ProxyChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// One in-test HTTP target: records every request it serves and answers with
/// the token-count JSON, or a redirect when `location` is set.
struct Target {
    requests: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    task: tokio::task::JoinHandle<()>,
    addr: SocketAddr,
}
impl Target {
    /// `bind` is the address the child will use to reach this server: the
    /// scenario picks `0.0.0.0` (no implicit NO_PROXY entry) or `127.0.0.1`.
    async fn start(bind: std::net::IpAddr, location: Option<String>) -> Self {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind((bind, 0))
            .await
            .expect("HTTP target needs listen permission");
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn({
            let requests = requests.clone();
            async move {
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    let requests = requests.clone();
                    let location = location.clone();
                    connections.spawn(async move {
                        let service = hyper::service::service_fn(
                            move |request: hyper::Request<hyper::body::Incoming>| {
                                let requests = requests.clone();
                                let location = location.clone();
                                async move {
                                    Ok::<_, Infallible>(
                                        serve_target(request, &requests, &location).await,
                                    )
                                }
                            },
                        );
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                            .await;
                    });
                }
            }
        });
        Self {
            requests,
            task,
            addr,
        }
    }
    fn url(&self) -> String {
        format!("http://{}/v1/messages/count_tokens", self.addr)
    }
    fn take_requests(&self) -> Vec<serde_json::Value> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}
impl Drop for Target {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_target(
    request: hyper::Request<hyper::body::Incoming>,
    requests: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    location: &Option<String>,
) -> hyper::Response<http_body_util::Full<bytes::Bytes>> {
    let (parts, body) = request.into_parts();
    let bytes = body
        .collect()
        .await
        .expect("target request body")
        .to_bytes();
    requests.lock().unwrap().push(serde_json::json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path(),
        "x-api-key": parts.headers.get("x-api-key").map(|v| v.to_str().unwrap()),
        "anthropic-version": parts
            .headers
            .get("anthropic-version")
            .map(|v| v.to_str().unwrap()),
        "content-type": parts
            .headers
            .get("content-type")
            .map(|v| v.to_str().unwrap()),
        "body": if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    }));
    if let Some(location) = location {
        return hyper::Response::builder()
            .status(307)
            .header("location", location)
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .unwrap();
    }
    hyper::Response::builder()
        .status(200)
        .body(http_body_util::Full::new(bytes::Bytes::from_static(
            br#"{"input_tokens":73}"#,
        )))
        .unwrap()
}

fn proxy_log(path: &Path) -> Vec<serde_json::Value> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Child: drive the production count transport against `PDRS_COUNT_URL`,
/// with the proxy environment the parent installed, and write the outcome.
async fn run_child() {
    let url = std::env::var("PDRS_COUNT_URL").expect("child url");
    let result_path = std::env::var("PDRS_COUNT_RESULT").expect("child result path");
    let mut headers = JsObject::new();
    headers.insert("content-type".into(), "application/json".into());
    headers.insert("x-api-key".into(), "count-proxy-test-key".into());
    headers.insert("anthropic-version".into(), "2023-06-01".into());
    let request = CountHttpRequest {
        url,
        headers,
        body: parse_json(
            r#"{"model":"count-proxy-model","messages":[{"role":"user","content":[{"type":"text","text":"count through the environment proxy"}]}]}"#,
        )
        .unwrap(),
    };
    let transport = HyperCountTransport::default();
    let value = match tokio::time::timeout(Duration::from_secs(10), transport.post(request)).await {
        Ok(Ok(value)) => {
            serde_json::from_str::<serde_json::Value>(&stringify_json(&value).unwrap().unwrap())
                .unwrap()
        }
        Ok(Err(error)) => serde_json::json!({ "error": error.message }),
        Err(_) => serde_json::json!({ "error": "child timeout" }),
    };
    std::fs::write(result_path, value.to_string()).unwrap();
}

/// Run one scenario child with exactly this proxy environment and return its
/// written outcome.
fn run_scenario(
    scratch: &Scratch,
    name: &str,
    url: &str,
    envs: &[(String, String)],
) -> serde_json::Value {
    let result_path = scratch.path().join(format!("{name}-result.json"));
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "count_transport_uses_environment_proxy",
            "--nocapture",
        ])
        .env_remove("PDRS_COUNT_URL")
        .env_remove("PDRS_COUNT_RESULT");
    for key in PROXY_ENV_KEYS {
        command.env_remove(key);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .env("PDRS_COUNT_URL", url)
        .env("PDRS_COUNT_RESULT", &result_path);
    let status = command.status().expect("spawn count proxy child");
    assert!(status.success(), "{name}: child failed: {status}");
    serde_json::from_str(&std::fs::read_to_string(&result_path).unwrap()).unwrap()
}

fn count_body() -> serde_json::Value {
    serde_json::json!({
        "model": "count-proxy-model",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "count through the environment proxy"}]}]
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn count_transport_uses_environment_proxy() {
    if std::env::var("PDRS_COUNT_URL").is_ok() {
        run_child().await;
        return;
    }
    let node = std::process::Command::new("node").arg("--version").output();
    assert!(
        node.is_ok_and(|output| output.status.success()),
        "the count proxy test needs `node` (>= 18) on PATH"
    );
    let scratch = Scratch::new("count-proxy");

    // HTTP_PROXY is picked up: the POST rides a CONNECT tunnel to the target
    // with its body and Anthropic auth headers intact.
    {
        let dir = scratch.path().join("proxied");
        std::fs::create_dir_all(&dir).unwrap();
        let (proxy, port, log) = ProxyChild::start(&dir, false, None).await;
        let target = Target::start(Ipv4Addr::UNSPECIFIED.into(), None).await;
        let outcome = run_scenario(
            &scratch,
            "proxied",
            &target.url(),
            &[("HTTP_PROXY".to_owned(), format!("http://127.0.0.1:{port}"))],
        );
        assert_eq!(
            outcome,
            serde_json::json!({"input_tokens": 73}),
            "proxied count must succeed"
        );
        let requests = target.take_requests();
        assert_eq!(requests.len(), 1, "target saw: {requests:?}");
        assert_eq!(requests[0]["method"], "POST");
        assert_eq!(requests[0]["path"], "/v1/messages/count_tokens");
        assert_eq!(requests[0]["x-api-key"], "count-proxy-test-key");
        assert_eq!(requests[0]["anthropic-version"], "2023-06-01");
        assert_eq!(requests[0]["content-type"], "application/json");
        assert_eq!(requests[0]["body"], count_body());
        let connects = proxy_log(&log);
        assert_eq!(connects.len(), 1, "connect log: {connects:?}");
        assert_eq!(connects[0]["method"], "CONNECT");
        assert_eq!(
            connects[0]["target"].as_str().unwrap(),
            target.addr.to_string(),
            "tunnel must target the count endpoint host"
        );
        drop(proxy);
    }

    // Proxy credentials from the URL reach the CONNECT as Basic auth.
    {
        let dir = scratch.path().join("proxy-auth");
        std::fs::create_dir_all(&dir).unwrap();
        let (proxy, port, log) = ProxyChild::start(&dir, true, None).await;
        let target = Target::start(Ipv4Addr::UNSPECIFIED.into(), None).await;
        let outcome = run_scenario(
            &scratch,
            "proxy-auth",
            &target.url(),
            &[(
                "HTTP_PROXY".to_owned(),
                format!("http://user:pass@127.0.0.1:{port}"),
            )],
        );
        assert_eq!(outcome, serde_json::json!({"input_tokens": 73}));
        assert_eq!(target.take_requests().len(), 1);
        let connects = proxy_log(&log);
        assert_eq!(connects.len(), 1);
        assert_eq!(
            connects[0]["proxyAuthorization"], "Basic <scrub>",
            "proxy URL credentials must authenticate the CONNECT"
        );
        drop(proxy);
    }

    // A 407 CONNECT refusal: no target traffic, plain `fetch failed`.
    {
        let dir = scratch.path().join("proxy-407");
        std::fs::create_dir_all(&dir).unwrap();
        let (proxy, port, log) = ProxyChild::start(&dir, false, Some(407)).await;
        let target = Target::start(Ipv4Addr::UNSPECIFIED.into(), None).await;
        let outcome = run_scenario(
            &scratch,
            "proxy-407",
            &target.url(),
            &[("HTTP_PROXY".to_owned(), format!("http://127.0.0.1:{port}"))],
        );
        assert_eq!(
            outcome,
            serde_json::json!({"error": "fetch failed"}),
            "407 must surface as the transport's fetch failed"
        );
        assert!(
            target.take_requests().is_empty(),
            "refused CONNECT must not reach the target"
        );
        assert_eq!(proxy_log(&log).len(), 1);
        drop(proxy);
    }

    // NO_PROXY covering the target host bypasses the proxy entirely.
    {
        let dir = scratch.path().join("no-proxy");
        std::fs::create_dir_all(&dir).unwrap();
        let (proxy, port, log) = ProxyChild::start(&dir, false, None).await;
        let target = Target::start(Ipv4Addr::UNSPECIFIED.into(), None).await;
        let outcome = run_scenario(
            &scratch,
            "no-proxy",
            &target.url(),
            &[
                ("HTTP_PROXY".to_owned(), format!("http://127.0.0.1:{port}")),
                ("NO_PROXY".to_owned(), "0.0.0.0".to_string()),
            ],
        );
        assert_eq!(outcome, serde_json::json!({"input_tokens": 73}));
        assert_eq!(target.take_requests().len(), 1, "request must go direct");
        assert!(
            proxy_log(&log).is_empty(),
            "NO_PROXY must keep the count transport off the proxy"
        );
        drop(proxy);
    }

    // A redirect re-evaluates the bypass per hop: the 0.0.0.0 first hop is
    // tunneled, the 127.0.0.1 redirect (implicit NO_PROXY entry) goes direct.
    {
        let dir = scratch.path().join("redirect");
        std::fs::create_dir_all(&dir).unwrap();
        let (proxy, port, log) = ProxyChild::start(&dir, false, None).await;
        let direct = Target::start(Ipv4Addr::LOCALHOST.into(), None).await;
        let redirect = Target::start(Ipv4Addr::UNSPECIFIED.into(), Some(direct.url())).await;
        let outcome = run_scenario(
            &scratch,
            "redirect",
            &redirect.url(),
            &[("HTTP_PROXY".to_owned(), format!("http://127.0.0.1:{port}"))],
        );
        assert_eq!(outcome, serde_json::json!({"input_tokens": 73}));
        assert_eq!(
            redirect.take_requests().len(),
            1,
            "first hop must answer exactly the redirected POST"
        );
        assert_eq!(direct.take_requests().len(), 1, "redirect hop must arrive");
        assert_eq!(
            proxy_log(&log).len(),
            1,
            "only the first hop goes through the proxy"
        );
        drop(proxy);
    }
}
