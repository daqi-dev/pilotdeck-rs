//! MCP `streamable_http` / legacy `sse` through the baseline's outgoing proxy.
//!
//! `tests/fixtures/mcp-proxy/generate.mts` ran the fixed baseline's real
//! `src/cli/proxy.ts` (`installGlobalProxy`, an undici `EnvHttpProxyAgent`)
//! in a fresh child per scenario, against a scripted local CONNECT proxy and
//! the shared scripted MCP server. This test replays the same scenarios with
//! the environment set per case, so the shared connector's env source,
//! `NO_PROXY` rule, CONNECT tunnel, proxy Basic auth and failure text are
//! compared to the TS oracle.
//!
//! The MCP target is addressed as `0.0.0.0` because the baseline always adds
//! `127.0.0.1` and `localhost` to `NO_PROXY`; `0.0.0.0` reaches the same local
//! server on this host without taking the implicit bypass. The proxy listens
//! on `127.0.0.1` and records one line per CONNECT.

#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::mcp::{
    client::{McpCallOptions, McpClient, McpClientOptions},
    protocol::{McpServerSpec, McpTransportSpec},
};
use pilotdeck_gateway::runtime::model::http::{ProxyInstallMode, ProxySettings};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use scratch::Scratch;
use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncBufReadExt;

static UNDEFINED: JsValue = JsValue::Undefined;
const ORACLE: &str = include_str!("fixtures/mcp-proxy/oracle.json");
const TLS: &str = include_str!("fixtures/mcp-proxy/proxy-tls.json");
const PROXY_ENV_KEYS: [&str; 7] = [
    "PILOTDECK_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "http_proxy",
    "HTTP_PROXY",
    "no_proxy",
    "NO_PROXY",
];

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
fn replace(text: &str, replacements: &[(String, String)]) -> String {
    let mut out = text.to_owned();
    for (needle, replacement) in replacements {
        out = out.replace(needle, replacement);
    }
    out
}
fn fill(value: &JsValue, replacements: &[(String, String)]) -> JsValue {
    map_strings(value, &|text| replace(text, replacements))
}
fn scrub(value: &JsValue, replacements: &[(String, String)]) -> JsValue {
    let reversed: Vec<(String, String)> = replacements
        .iter()
        .map(|(needle, replacement)| (replacement.clone(), needle.clone()))
        .collect();
    map_strings(value, &|text| replace(text, &reversed))
}
fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
fn tls_roots() -> rustls::RootCertStore {
    let material = parse_json(TLS).unwrap();
    let ca =
        CertificateDer::from_pem_slice(string(field(&material, "ca"), "ca").as_bytes()).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    roots
}

// ------------------------------------------------------------- child servers

struct Child {
    child: tokio::process::Child,
    port: u16,
}
impl Child {
    async fn start_node(args: &[&str], dir: &Path, envs: &[(&str, String)]) -> Self {
        let mut command = tokio::process::Command::new("node");
        command
            .args(args)
            .env("MOCK_DIR", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn node helper");
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let line = lines.next_line().await.unwrap().expect("PORT line");
        let port = line
            .strip_prefix("PORT ")
            .expect("PORT line")
            .parse()
            .unwrap();
        Self { child, port }
    }
    async fn start_mock(dir: &Path, tls: Option<&Path>) -> Self {
        let mut envs = vec![("MOCK_HOST", "0.0.0.0".to_owned())];
        if let Some(tls) = tls {
            envs.push(("MOCK_TLS", tls.to_string_lossy().into_owned()));
        }
        Self::start_node(
            &[
                fixture_path("tests/fixtures/mcp-client/mock_server.mjs")
                    .to_string_lossy()
                    .as_ref(),
                "--http",
            ],
            dir,
            &envs,
        )
        .await
    }
    async fn start_proxy(dir: &Path, mode: &str) -> Self {
        let mut envs = vec![(
            "PROXY_LOG",
            dir.join("proxy.jsonl").to_string_lossy().into_owned(),
        )];
        if mode == "auth" {
            envs.push(("PROXY_AUTH", "user:pass".to_owned()));
        }
        if mode == "reject" {
            envs.push(("PROXY_STATUS", "403".to_owned()));
        }
        Self::start_node(
            &[fixture_path("tests/fixtures/mcp-proxy/connect_proxy.mjs")
                .to_string_lossy()
                .as_ref()],
            dir,
            &envs,
        )
        .await
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// A loopback port that is closed again before use.
fn closed_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// ------------------------------------------------------------------ scenario

fn proxy_url(proxy: &JsValue, port: u16) -> String {
    if string(field(proxy, "mode"), "proxy mode") != "auth" {
        return format!("http://127.0.0.1:{port}");
    }
    let user = string(field(proxy, "authUser"), "authUser");
    let pass = if string(field(proxy, "urlAuth"), "urlAuth") == "good" {
        string(field(proxy, "authPass"), "authPass")
    } else {
        "wrong".to_owned()
    };
    format!("http://{user}:{pass}@127.0.0.1:{port}")
}

/// The environment for one case, exactly as the TS parent set it for its
/// worker (see `generate.mts`). Returned rather than installed: the replay
/// re-execs the test binary with this environment, because the workspace
/// forbids `unsafe` and edition 2024 makes `set_var` unsafe.
fn case_env(
    case: &JsValue,
    mock_port: u16,
    proxy_port: u16,
    closed_port: u16,
) -> Vec<(String, String)> {
    let replacements = vec![
        ("<mock>".to_owned(), format!("0.0.0.0:{mock_port}")),
        (
            "<proxy>".to_owned(),
            proxy_url(field(case, "proxy"), proxy_port),
        ),
        (
            "<closedProxy>".to_owned(),
            format!("http://127.0.0.1:{closed_port}"),
        ),
    ];
    let proxy = field(case, "proxy");
    let mut envs: Vec<(String, String)> = Vec::new();
    if let Some(key) = field(proxy, "env").as_string() {
        envs.push((key.to_utf8().unwrap(), replace("<proxy>", &replacements)));
    }
    if let Some(extra) = field(case, "env").as_object() {
        for (key, value) in extra.own_entries() {
            envs.push((
                key.to_utf8().unwrap(),
                string(&fill(value, &replacements), "env"),
            ));
        }
    }
    let no_proxy = field(case, "noProxy");
    if no_proxy.as_object().is_some() {
        envs.push((
            string(field(no_proxy, "name"), "noProxy name"),
            string(
                &fill(field(no_proxy, "value"), &replacements),
                "noProxy value",
            ),
        ));
    }
    envs
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
fn options(value: &JsValue, proxy: Option<ProxySettings>) -> McpClientOptions {
    McpClientOptions {
        call_timeout_ms: field(value, "callTimeoutMs").as_number(),
        handshake_timeout_ms: field(value, "handshakeTimeoutMs").as_number(),
        proxy,
    }
}

/// The `pilotdeck.yaml` source for a case, with placeholders resolved.
fn proxy_settings(case: &JsValue, replacements: &[(String, String)]) -> Option<ProxySettings> {
    let config = field(case, "config");
    config.as_object()?;
    let url = string(&fill(field(config, "url"), replacements), "config url");
    let no_proxy = field(config, "noProxy").as_string().map(|_| {
        string(
            &fill(field(config, "noProxy"), replacements),
            "config noProxy",
        )
    });
    let mode = match string(field(case, "configMode"), "configMode").as_str() {
        "startup" => ProxyInstallMode::Startup,
        "reload" => ProxyInstallMode::Reload,
        other => panic!("unknown config mode {other}"),
    };
    Some(ProxySettings {
        url,
        no_proxy,
        mode,
    })
}

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

fn json_lines(path: &Path, replacements: &[(String, String)], dedupe: bool) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut entries: Vec<String> = content
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| text(&scrub(&parse_json(line).unwrap(), replacements)))
        .collect();
    entries.sort();
    if dedupe {
        entries.dedup();
    }
    entries
}

/// Child process: drive the client with the environment its parent installed.
async fn run_child(index: usize) {
    let oracle = oracle();
    let cases = array(field(&oracle, "cases"), "cases");
    let case = &cases[index];
    let url = std::env::var("PDRS_MCP_PROXY_URL").expect("child url");
    let use_tls = std::env::var("PDRS_MCP_PROXY_TLS").as_deref() == Ok("1");
    let proxy = std::env::var("PDRS_MCP_PROXY_CONFIG_URL")
        .ok()
        .map(|url| ProxySettings {
            url,
            no_proxy: std::env::var("PDRS_MCP_PROXY_CONFIG_NO_PROXY").ok(),
            mode: if std::env::var("PDRS_MCP_PROXY_CONFIG_MODE").as_deref() == Ok("reload") {
                ProxyInstallMode::Reload
            } else {
                ProxyInstallMode::Startup
            },
        });
    let client = McpClient::new(spec(case, url), options(field(case, "options"), proxy));
    let client = if use_tls {
        client.with_tls_roots(tls_roots())
    } else {
        client
    };
    let mut steps = Vec::new();
    for op in array(field(case, "ops"), "ops") {
        let mut step: Fields = vec![("step", op.clone())];
        step.extend(run_op(client.clone(), op).await);
        step.push(("status", JsValue::String(client.status().as_str().into())));
        steps.push(object(step));
    }
    let result = std::env::var("PDRS_MCP_PROXY_RESULT").expect("child result path");
    std::fs::write(result, text(&JsValue::Array(steps))).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replays_ts_mcp_http_proxy_scenarios() {
    if let Ok(index) = std::env::var("PDRS_MCP_PROXY_CASE") {
        run_child(index.parse().expect("case index")).await;
        return;
    }
    let node = std::process::Command::new("node").arg("--version").output();
    assert!(
        node.is_ok_and(|output| output.status.success()),
        "the MCP proxy replay needs `node` (>= 18) on PATH"
    );
    let oracle = oracle();
    let cases = array(field(&oracle, "cases"), "cases");
    assert_eq!(cases.len(), 35);
    let scratch = Scratch::new("mcp-proxy");
    let mut steps_seen = 0usize;
    let mut requests_seen = 0usize;
    let mut connects_seen = 0usize;
    for (index, case) in cases.iter().enumerate() {
        let id = string(field(case, "id"), "id");
        let dir = scratch.path().join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("script.json"),
            text(&object(vec![("http", field(case, "script").clone())])),
        )
        .unwrap();
        let tls_path = fixture_path("tests/fixtures/mcp-proxy/proxy-tls.json");
        let use_tls = field(case, "mockTls").as_bool().unwrap_or(false);
        let mock = Child::start_mock(&dir, use_tls.then_some(tls_path.as_path())).await;
        let mode = string(field(field(case, "proxy"), "mode"), "proxy mode");
        let proxy = if mode == "closed" {
            None
        } else {
            Some(Child::start_proxy(&dir, &mode).await)
        };
        // Allocate the closed port after the live proxy holds its socket, so
        // the two can never coincide.
        let closed = closed_port();
        let proxy_port = proxy.as_ref().map_or(closed, |proxy| proxy.port);
        // Same placeholder set and order as `generate.mts`; `scrub` applies
        // the inverse, so the full URL must be replaced before its authority.
        let replacements = vec![
            ("<mock>".to_owned(), format!("0.0.0.0:{}", mock.port)),
            (
                "<proxy>".to_owned(),
                proxy_url(field(case, "proxy"), proxy_port),
            ),
            (
                "<closedProxy>".to_owned(),
                format!("http://127.0.0.1:{closed}"),
            ),
            (
                "<proxyAuthority>".to_owned(),
                format!("127.0.0.1:{proxy_port}"),
            ),
            (
                "<closedAuthority>".to_owned(),
                format!("127.0.0.1:{closed}"),
            ),
            (
                "<mockAuthority>".to_owned(),
                format!("0.0.0.0:{}", mock.port),
            ),
        ];
        let url = replace(&string(field(case, "url"), "url"), &replacements);
        let settings = proxy_settings(case, &replacements);
        let result_path = dir.join("rust-steps.json");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "replays_ts_mcp_http_proxy_scenarios",
                "--nocapture",
            ])
            .env_remove("PDRS_MCP_PROXY_CASE")
            .env_remove("PDRS_MCP_PROXY_URL")
            .env_remove("PDRS_MCP_PROXY_TLS")
            .env_remove("PDRS_MCP_PROXY_CONFIG_URL")
            .env_remove("PDRS_MCP_PROXY_CONFIG_NO_PROXY")
            .env_remove("PDRS_MCP_PROXY_CONFIG_MODE")
            .env_remove("PDRS_MCP_PROXY_RESULT");
        for key in PROXY_ENV_KEYS {
            command.env_remove(key);
        }
        for (key, value) in case_env(case, mock.port, proxy_port, closed) {
            command.env(key, value);
        }
        if let Some(settings) = &settings {
            command.env("PDRS_MCP_PROXY_CONFIG_URL", &settings.url).env(
                "PDRS_MCP_PROXY_CONFIG_MODE",
                match settings.mode {
                    ProxyInstallMode::Startup => "startup",
                    ProxyInstallMode::Reload => "reload",
                },
            );
            if let Some(no_proxy) = &settings.no_proxy {
                command.env("PDRS_MCP_PROXY_CONFIG_NO_PROXY", no_proxy);
            }
        }
        command
            .env("PDRS_MCP_PROXY_CASE", index.to_string())
            .env("PDRS_MCP_PROXY_URL", &url)
            .env("PDRS_MCP_PROXY_TLS", if use_tls { "1" } else { "0" })
            .env("PDRS_MCP_PROXY_RESULT", &result_path);
        let status = command.status().expect("spawn replay child");
        assert!(status.success(), "{id}: replay child failed: {status}");
        let actual = parse_json(&std::fs::read_to_string(&result_path).unwrap()).unwrap();
        let actual = array(&actual, "steps");
        let expected = array(field(case, "steps"), "steps");
        assert_eq!(actual.len(), expected.len(), "{id}: step count");
        for (step_index, step) in actual.iter().enumerate() {
            assert_eq!(
                text(&scrub(step, &replacements)),
                text(&expected[step_index]),
                "{id}: step {step_index}"
            );
            steps_seen += 1;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let requests = json_lines(&dir.join("http.jsonl"), &replacements, false);
        let expected: Vec<String> = array(field(case, "requests"), "requests")
            .iter()
            .map(text)
            .collect();
        assert_eq!(requests, expected, "{id}: server request log");
        // The target-side proof for the P0 fix: a caller-supplied
        // `Proxy-Authorization` is either refused before dispatch (the two
        // proxied cases, whose request log is empty) or, on the direct
        // `NO_PROXY` path, forwarded exactly like TS does. Either way it must
        // never arrive through the tunnel; `mock_server.mjs` logs the header
        // lowercased, so the check is case-insensitive.
        let proxy_auth_case = pairs(field(case, "headers")).is_some_and(|headers| {
            headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("proxy-authorization"))
        });
        if proxy_auth_case && id != "proxy-auth-header-direct" {
            assert!(
                requests
                    .iter()
                    .all(|line| !line.to_ascii_lowercase().contains("proxy-authorization")),
                "{id}: target received Proxy-Authorization through the proxy"
            );
            assert!(requests.is_empty(), "{id}: refused request reached target");
        }
        if id == "proxy-auth-header-direct" {
            // The direct agent has no undici check, so the header does arrive
            // (and the oracle pins the same bytes).
            assert!(
                requests
                    .iter()
                    .any(|line| line.contains("\"proxy-authorization\":\"Basic abc\"")),
                "direct path must preserve the caller header"
            );
        }
        requests_seen += requests.len();
        let connects = json_lines(&dir.join("proxy.jsonl"), &replacements, true);
        let expected: Vec<String> = array(field(case, "connects"), "connects")
            .iter()
            .map(text)
            .collect();
        assert_eq!(connects, expected, "{id}: proxy CONNECT log");
        connects_seen += connects.len();
        drop(proxy);
        drop(mock);
    }
    assert_eq!(steps_seen, 133);
    assert_eq!(requests_seen, 140);
    assert_eq!(connects_seen, 23);
}

/// Re-runs the real TS generator and requires a byte-identical oracle.
#[test]
#[ignore]
fn mcp_proxy_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("mcp-proxy");
}
