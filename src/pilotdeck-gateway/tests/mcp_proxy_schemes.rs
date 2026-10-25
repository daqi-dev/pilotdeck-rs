//! Real TS oracle, local proxy wire logs and target receipts. Each environment
//! is isolated in a child (no unsafe process-global environment mutation).
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::runtime::{
    mcp::{
        client::{McpClient, McpClientOptions},
        protocol::{McpServerSpec, McpTransportSpec},
    },
    model::http::{PROXY_ENV_KEYS, ProxyInstallMode, ProxySettings},
};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncBufReadExt;
const ORACLE: &str = include_str!("fixtures/mcp-proxy-schemes/oracle.json");
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-proxy-schemes")
        .join(name)
}
fn lines(dir: &Path, name: &str) -> Vec<Value> {
    std::fs::read_to_string(dir.join(format!("{name}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn roots() -> rustls::RootCertStore {
    let material: Value =
        serde_json::from_str(include_str!("fixtures/mcp-proxy-schemes/tls.json")).unwrap();
    let ca = CertificateDer::from_pem_slice(material["ca"].as_str().unwrap().as_bytes()).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    roots
}
async fn child(spec: &Value) {
    let url = std::env::var("PDRS_SCHEME_URL").unwrap();
    let proxy = std::env::var("PDRS_SCHEME_PROXY").unwrap();
    let install = spec["install"].as_str().unwrap_or("");
    let settings = match install {
        "fallback" | "reload" => Some(ProxySettings {
            url: if install == "fallback" {
                proxy
            } else {
                spec["invalid"].as_str().unwrap().into()
            },
            mode: if install == "reload" {
                ProxyInstallMode::Reload
            } else {
                ProxyInstallMode::Startup
            },
            no_proxy: None,
        }),
        _ => None,
    };
    let transport = if spec["transport"] == "sse" {
        McpTransportSpec::Sse { url, headers: None }
    } else {
        McpTransportSpec::StreamableHttp { url, headers: None }
    };
    let client = McpClient::new(
        McpServerSpec {
            id: "local".into(),
            transport,
            call_timeout_ms: None,
        },
        McpClientOptions {
            proxy: settings,
            handshake_timeout_ms: Some(2000.0),
            call_timeout_ms: Some(2000.0),
        },
    );
    let client = if spec["trust"] != false {
        client.with_tls_roots(roots())
    } else {
        client
    };
    let result = async {
        client.start().await?;
        client.list_tools().await
    }
    .await;
    let result = match result {
        Ok(tools) => {
            json!({"ok": true, "tools": tools.iter().map(|tool| tool.tool_name.to_utf8().unwrap()).collect::<Vec<_>>()})
        }
        Err(error) => json!({"ok": false, "error": error.message()}),
    };
    client.close().await;
    std::fs::write(
        std::env::var("PDRS_SCHEME_RESULT").unwrap(),
        result.to_string(),
    )
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replays_ts_proxy_schemes() {
    baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    if let Ok(id) = std::env::var("PDRS_SCHEME_CHILD") {
        let spec = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|spec| spec["id"] == id)
            .unwrap();
        child(spec).await;
        return;
    }
    let root = std::env::var_os("PDRS_SCHEME_EVIDENCE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = root.join(format!("proxy-schemes-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    for spec in oracle["cases"].as_array().unwrap() {
        let id = spec["id"].as_str().unwrap();
        if std::env::var("PDRS_SCHEME_FILTER").is_ok_and(|filter| !id.contains(&filter)) {
            continue;
        }
        let dir = root.join(id);
        std::fs::create_dir(&dir).unwrap();
        let mut server = tokio::process::Command::new("node")
            .arg(fixture("net.mjs"))
            .args(["--serve", &spec.to_string()])
            .arg(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let line = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::io::BufReader::new(server.stdout.take().unwrap())
                .lines()
                .next_line()
                .await
                .unwrap()
                .unwrap()
        })
        .await
        .expect("local server ready");
        let ports: Value = serde_json::from_str(&line).unwrap();
        let scheme = spec["scheme"].as_str().unwrap();
        let auth = spec["auth"]
            .as_str()
            .map(|auth| {
                format!(
                    "{}@",
                    if spec["badAuth"] == true {
                        "wrong:wrong"
                    } else {
                        auth
                    }
                )
            })
            .unwrap_or_default();
        let proxy = format!("{scheme}://{auth}127.0.0.1:{}", ports["proxy"]);
        let host = if spec["install"] == "invalid" {
            "127.0.0.1"
        } else {
            "mcp.test"
        };
        let url = format!(
            "{}://{host}:{}/mcp",
            if spec["targetTls"] == true {
                "https"
            } else {
                "http"
            },
            ports["target"]
        );
        let output = dir.join("rust.json");
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "replays_ts_proxy_schemes", "--nocapture"])
            .env("PDRS_SCHEME_CHILD", id)
            .env("PDRS_SCHEME_URL", url)
            .env("PDRS_SCHEME_PROXY", &proxy)
            .env("PDRS_SCHEME_RESULT", &output)
            .kill_on_drop(true);
        for key in PROXY_ENV_KEYS {
            command.env_remove(key);
        }
        command.env(
            "PILOTDECK_PROXY",
            if spec["install"] == "invalid" || spec["install"] == "fallback" {
                spec["invalid"].as_str().unwrap()
            } else {
                &proxy
            },
        );
        let result = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .expect("replay deadline")
            .unwrap();
        std::fs::write(
            dir.join("rust.log"),
            [&result.stdout[..], &result.stderr[..]].concat(),
        )
        .unwrap();
        assert!(
            result.status.success(),
            "{id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let actual: Value =
            serde_json::from_str(&std::fs::read_to_string(output).unwrap()).unwrap();
        for name in ["handshake", "auth", "connection"] {
            let mut entries = Vec::new();
            for entry in lines(&dir, name) {
                if !entries.contains(&entry) {
                    entries.push(entry);
                }
            }
            let expected = if spec["failClosed"] == true {
                json!([])
            } else {
                spec["ts"][name].clone()
            };
            assert_eq!(json!(entries), expected, "{id}: {name}");
        }
        let targets = lines(&dir, "target");
        let proxies = lines(&dir, "proxy");
        assert!(
            targets.iter().all(|entry| entry["proxyAuth"].is_null()),
            "{id}: credential leak"
        );
        if spec["failClosed"] == true {
            assert_eq!(actual["ok"], false, "{id}: {actual}");
            // Streamable MCP intentionally projects the public fetch failure;
            // the shared-connector unit test asserts the configuration cause.
            assert_eq!(
                actual["error"], "MCP handshake failed: fetch failed",
                "{id}"
            );
            assert!(
                targets.is_empty() && proxies.is_empty(),
                "{id}: invalid config touched network"
            );
        } else {
            assert_eq!(actual["ok"], spec["ts"]["ok"], "{id}: {actual}");
            if actual["ok"] == true {
                assert_eq!(actual["tools"], spec["ts"]["tools"], "{id}");
            }
            let mut unique = Vec::new();
            for entry in proxies {
                if !unique.contains(&entry) {
                    unique.push(entry);
                }
            }
            assert_eq!(json!(unique), spec["ts"]["proxy"], "{id}: proxy wire");
            let mut methods: Vec<_> = targets
                .iter()
                .filter_map(|entry| entry["method"].as_str())
                .filter(|method| ["initialize", "tools/list"].contains(method))
                .collect();
            methods.sort();
            methods.dedup();
            assert_eq!(
                json!(methods),
                spec["ts"]["methods"],
                "{id}: target receipts"
            );
        }
        server.kill().await.unwrap();
        server.wait().await.unwrap();
        println!("PASS {id}: {actual}");
    }
    println!("wire evidence: {}", root.display());
}
