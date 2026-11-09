//! Isolated subprocess coverage for the standalone TS-compatible Web entry.
#[path = "common/scratch.rs"]
mod scratch;
use scratch::Scratch;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pilotdeck-web"));
    command
        .env_clear()
        .env("HOME", root)
        .env("PILOT_HOME", root)
        .env("HOST", "127.0.0.1")
        .env("PILOTDECK_GATEWAY_TOKEN", "isolated-web-cli-token")
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root);
    command
}
fn text(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn web_cli_status_info_options_and_environment_are_process_local() {
    let root = Scratch::new("web-cli-status");
    let default = text(command(root.path()).arg("status").output().unwrap());
    assert!(default.contains("Version: 0.1.0"));
    assert!(default.contains("Installation Directory:"));
    assert!(default.contains("Server Port: 3001"));
    assert!(default.contains(&format!(
        "Config File: {}",
        root.path().join("pilotdeck.yaml").display()
    )));
    assert!(default.contains("[WARN] Not found"));
    assert!(default.contains("Onboarding: [WARN] Required"));
    assert!(default.contains("[WARN] Not created yet"));
    assert!(!root.path().join("auth.db").exists());
    assert!(!root.path().join("pilotdeck.yaml").exists());
    let info = text(
        command(root.path())
            .arg("info")
            .env("PORT", "43123")
            .output()
            .unwrap(),
    );
    assert!(info.contains("Server Port: 43123"));
    let overridden = text(
        command(root.path())
            .args([
                "status",
                "--port=43124",
                "--database-path=data.db",
                "--config=config.yaml",
            ])
            .env("SERVER_PORT", "43125")
            .env("PORT", "43126")
            .output()
            .unwrap(),
    );
    assert!(overridden.contains("Server Port: 43124"));
    assert!(overridden.contains("Database: data.db"));
    assert!(overridden.contains("Config File: config.yaml"));
    let environment = text(
        command(root.path())
            .arg("status")
            .env("SERVER_PORT", "43125")
            .env("PORT", "43126")
            .output()
            .unwrap(),
    );
    assert!(environment.contains("Server Port: 43125"));
    let split = text(
        command(root.path())
            .args([
                "status",
                "-p",
                "43127",
                "--database-path",
                "split.db",
                "--config",
                "split.yaml",
            ])
            .output()
            .unwrap(),
    );
    assert!(split.contains("Server Port: 43127"));
    assert!(split.contains("Database: split.db"));
    assert!(split.contains("Config File: split.yaml"));
}

#[test]
fn web_cli_status_preserves_baseline_onboarding_gate_without_printing_config() {
    let root = Scratch::new("web-cli-onboarding");
    let config = root.path().join("pilotdeck.yaml");
    std::fs::write(&config, "schemaVersion: 1\nagent: {model: fixture/base}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: status-fixture-marker\n      models: {base: {}}\n").unwrap();
    std::fs::write(root.path().join("auth.db"), b"database existence fixture").unwrap();
    let modern = text(command(root.path()).arg("status").output().unwrap());
    assert!(modern.contains("Onboarding: [WARN] Required"));
    assert!(modern.contains("Status: [OK] Exists"));
    assert!(!modern.contains("status-fixture-marker"));
    assert!(!modern.contains("apiKey"));
    std::fs::write(&config, "agents: {main: {model: legacy}}\nmodels:\n  entries: {legacy: {name: base, provider: fixture}}\n  providers: {fixture: {baseUrl: http://127.0.0.1:1, apiKey: status-fixture-marker}}\n").unwrap();
    let legacy = text(command(root.path()).arg("status").output().unwrap());
    assert!(legacy.contains("Onboarding: [OK] Complete"));
    assert!(!legacy.contains("status-fixture-marker"));
    let legacy_source = std::fs::read_to_string(&config).unwrap();
    for invalid in [
        "gateway: {bindAddress: 0.0.0.0}\n",
        "tools: {webSearch: {enabled: true, provider: unsupported}}\n",
        "router: {enabled: true, tokenSaver: {judge: missing/model}}\n",
        "model: {providers: {fixture: {protocol: openai, url: http://localhost, apiKey: marker, models: {base: {thinking: {state: invalid}}}}}}\n",
    ] {
        std::fs::write(&config, format!("{legacy_source}{invalid}")).unwrap();
        let invalid = text(command(root.path()).arg("status").output().unwrap());
        assert!(invalid.contains("Onboarding: [WARN] Required"));
    }
    std::fs::write(&config, "agents: [broken\n").unwrap();
    let malformed = text(command(root.path()).arg("status").output().unwrap());
    assert!(malformed.contains("Onboarding: [WARN] Required"));
    std::fs::write(&config, "").unwrap();
    let empty = text(command(root.path()).arg("status").output().unwrap());
    assert!(empty.contains("Onboarding: [WARN] Required"));
}

#[test]
fn web_cli_help_version_unknown_command_and_port_failure_exit_codes() {
    let root = Scratch::new("web-cli-commands");
    for args in [vec!["help"], vec!["--help"], vec!["-h"]] {
        let help = text(command(root.path()).args(args).output().unwrap());
        assert!(help.contains("pilotdeck [command] [options]"));
        assert!(help.contains("--database-path <path>"));
    }
    for args in [vec!["version"], vec!["--version"], vec!["-v"]] {
        assert_eq!(
            text(command(root.path()).args(args).output().unwrap()),
            "0.1.0\n"
        );
    }
    let unknown = command(root.path()).arg("unknown").output().unwrap();
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("Unknown command: unknown"));
    let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = occupied.local_addr().unwrap().port().to_string();
    let output = command(root.path())
        .args(["--port", &port])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&format!("Port {port} is already in use"))
    );
    for port in ["invalid", "-1", "65536", "1.5"] {
        let output = command(root.path())
            .args(["start", "--port", port])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid server port"));
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn exchange(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(200)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[tokio::test]
async fn web_cli_default_and_start_mount_native_http_static_and_authenticated_ws() {
    use pilotdeck_gateway::cron::client::GatewayClient;
    for explicit_start in [false, true] {
        let root = Scratch::new("web-cli-start");
        let assets = root.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(
            assets.join("index.html"),
            "<!doctype html><title>isolated native web</title>",
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut start = command(root.path());
        if explicit_start {
            start.arg("start");
        }
        start
            .env("PORT", address.port().to_string())
            .env(
                "PILOTDECK_PROXY_UPSTREAM",
                "invalid legacy upstream must be ignored",
            )
            .env("PILOTDECK_STATIC_ASSETS_PATH", &assets)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut process = Process(start.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "native web CLI did not listen");
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "native web CLI exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let health = exchange(address, "/health");
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        assert!(health.contains(r#"{"ok":true}"#));
        let index = exchange(address, "/");
        assert!(index.contains("isolated native web"), "{index}");
        let client =
            GatewayClient::connect(&format!("http://{address}/ws"), "isolated-web-cli-token")
                .await
                .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            client.request("not_a_gateway_method", &serde_json::json!({})),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.message.contains("not_a_gateway_method"));
        client.close().await;
        assert!(
            !root.path().join("pilotdeck.yaml").exists(),
            "empty configuration starts without creating one"
        );
    }
}
