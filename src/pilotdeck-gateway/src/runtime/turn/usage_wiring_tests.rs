use super::*;
use crate::{runtime::connection::GatewayTextConnection, test_scratch::Scratch};
use pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION;
use serde_json::{Value, json};
use std::{
    fs,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
struct Text(Mutex<Vec<Value>>);
impl GatewayTextConnection for Text {
    fn send_text(&self, message: String) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::from_str(&message).unwrap());
    }
    fn close(&self, code: u16, reason: &str) {
        panic!("unexpected connection close {code}: {reason}");
    }
}

#[derive(Default)]
struct NoSidecar(AtomicUsize);
impl SidecarSpawner for NoSidecar {
    fn command(&self) -> std::io::Result<Command> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other(
            "usage query must not start a sidecar",
        ))
    }
}

fn row(session: &str, timestamp: &str) -> String {
    json!({
        "sessionId": session, "turnId": "historical",
        "provider": "proof", "model": "base",
        "scenarioType": "explicit", "role": "main",
        "usage": {"inputTokens": 10, "outputTokens": 2, "totalTokens": 12},
        "startedAt": timestamp, "endedAt": timestamp
    })
    .to_string()
}

async fn query(
    connection: &super::super::gateway::HostedConnection,
    text: &Text,
    id: &str,
    method: &str,
    params: Value,
) -> Value {
    tokio::time::timeout(
        Duration::from_secs(5),
        connection.receive(
            json!({"type": "request", "id": id, "method": method, "params": params}).to_string(),
        ),
    )
    .await
    .expect("usage RPC must finish")
    .unwrap();
    text.0
        .lock()
        .unwrap()
        .iter()
        .find(|frame| frame["id"] == id)
        .expect("wire response")
        .clone()
}

async fn first_query_with_project_stats(stats: &str, expected: u64) {
    let scratch = Scratch::new("usage-wiring");
    let home = scratch.path().join("home");
    let project = scratch.path().join("project");
    fs::create_dir_all(home.join("router")).unwrap();
    fs::create_dir_all(project.join(".pilotdeck")).unwrap();
    fs::write(
        home.join("pilotdeck.yaml"),
        "schemaVersion: 1\nagent: {model: proof/base}\nrouter: {enabled: true}\nmodel:\n  providers:\n    proof:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: public-test-only\n      models: {base: {}}\n",
    )
    .unwrap();
    fs::write(
        project.join(".pilotdeck/pilotdeck.yaml"),
        format!("schemaVersion: 1\nrouter:\n  enabled: true\n  stats: {stats}\n"),
    )
    .unwrap();
    let journal = home.join("router/stats.jsonl");
    let original = format!(
        "{}\n{}\n",
        row("expired", "2000-01-01T00:00:00.000Z"),
        row("retained", "2999-01-01T00:00:00.000Z"),
    );
    fs::write(&journal, &original).unwrap();
    let providers_home = home.clone();
    let providers_cwd = scratch.path().to_path_buf();
    let spawner = Arc::new(NoSidecar::default());
    let composition = compose(CompositionOptions {
        policies: Default::default(),
        activity: Default::default(),
        background: Arc::new(
            crate::cron::runtime::BackgroundService::new(std::env::temp_dir()).unwrap(),
        ),
        cron: Default::default(),
        pilot_home: home,
        cwd: scratch.path().to_string_lossy().as_ref().into(),
        spawner: spawner.clone(),
        providers: Box::new(move |owner| disk_providers(providers_home, providers_cwd, owner)),
        failure_guard_enabled: false,
        abort_turn_timeout_ms: 1_000,
        server_version: "usage-test".into(),
    })
    .unwrap();
    let text = Arc::new(Text::default());
    let connection = composition
        .turn
        .connect(text.clone(), "test-token".into(), "test".into());
    connection
        .receive(
            json!({
                "type": "hello", "protocolVersion": GATEWAY_PROTOCOL_VERSION,
                "clientName": "usage-test", "clientVersion": "1", "token": "test-token"
            })
            .to_string(),
        )
        .await
        .unwrap();
    assert!(
        text.0
            .lock()
            .unwrap()
            .iter()
            .any(|frame| frame["type"] == "hello_ok")
    );

    // Both methods must respect the project before any submit/session creation.
    // Reverse the first RPC so either method independently catches the regression.
    let methods = if expected == 0 {
        ["usage_snapshot", "model_usage_snapshot"]
    } else {
        ["model_usage_snapshot", "usage_snapshot"]
    };
    for method in methods {
        let response = query(
            &connection,
            &text,
            method,
            method,
            json!({"projectKey": project}),
        )
        .await;
        assert_eq!(response["ok"], true, "{response}");
        let result = &response["result"];
        assert_eq!(result["scope"], "project");
        if method == "usage_snapshot" {
            assert_eq!(
                result["aggregate"]["totalRequests"].as_f64(),
                Some(expected as f64)
            );
        } else {
            let models = result["models"].as_array().unwrap();
            assert_eq!(models.len(), usize::from(expected != 0));
            if expected != 0 {
                assert_eq!(models[0]["totalRequests"].as_f64(), Some(expected as f64));
            }
        }
    }
    let expired = query(
        &connection,
        &text,
        "expired",
        "usage_snapshot",
        json!({"projectKey": project, "sessionKey": "expired"}),
    )
    .await;
    assert_eq!(expired["ok"], true, "{expired}");
    assert_eq!(expired["result"]["scope"], "project");
    let retained = query(
        &connection,
        &text,
        "retained",
        "model_usage_snapshot",
        json!({"projectKey": project, "sessionKey": "retained"}),
    )
    .await;
    assert_eq!(retained["ok"], true, "{retained}");
    assert_eq!(
        retained["result"]["scope"],
        if expected == 0 { "project" } else { "session" }
    );
    assert_eq!(spawner.0.load(Ordering::SeqCst), 0);
    if expected == 0 {
        assert_eq!(fs::read_to_string(&journal).unwrap(), original);
    } else {
        let persisted = fs::read_to_string(&journal).unwrap();
        assert!(!persisted.contains("expired"));
        assert!(persisted.contains("retained"));
    }
    connection.closed().await.unwrap();
}

#[tokio::test]
async fn first_usage_rpc_honors_disabled_project_stats_with_existing_journal() {
    first_query_with_project_stats("{enabled: false}", 0).await;
}

#[tokio::test]
async fn first_model_usage_rpc_honors_project_retention_with_existing_journal() {
    first_query_with_project_stats("{enabled: true, retentionMs: 60000}", 1).await;
}
