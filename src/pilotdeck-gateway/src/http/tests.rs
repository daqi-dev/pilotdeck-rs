use super::*;
use crate::LocalServer;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

fn server() -> Arc<LocalServer> {
    let proxy = std::sync::Arc::new(crate::http_proxy::Proxy {
        upstream: "http://127.0.0.1:1".into(),
        client: hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http(),
    });
    let mut server = LocalServer::with_token(proxy, "isolated-test-token".into()).unwrap();
    server.http = HttpState::new("isolated-test-token".into(), None, std::env::temp_dir());
    Arc::new(server)
}

async fn request(method: &str, path: &str, auth: Option<&str>, body: &str) -> (StatusCode, String) {
    request_to(server(), method, path, auth, body).await
}

async fn request_to(
    server: Arc<LocalServer>,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let (server_io, client_io) = tokio::io::duplex(65536);
    let job = tokio::spawn(async move { server.serve_connection(server_io).await });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client_io))
        .await
        .unwrap();
    let client = tokio::spawn(connection);
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    let response = sender
        .send_request(
            builder
                .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    client.await.unwrap().unwrap();
    job.await.unwrap().unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn config_http_uses_authenticated_shared_owner_for_read_validate_and_save() {
    let root = std::env::temp_dir().join(format!("pdrs-http-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("pilotdeck.yaml"),
        "# keep\nschemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1/v1\n      apiKey: secret\n      models: {deterministic: {}}\n",
    )
    .unwrap();
    let mut local = Arc::try_unwrap(server()).ok().unwrap();
    local.http.home = Some(root.clone());
    local.http.cwd = root.clone();
    local.http.config = Some(crate::config::routes::native::install(
        crate::config::routes::ConfigRoutes::new(
            root.clone(),
            root.clone(),
            crate::config::Env::from([("HOME".into(), root.to_string_lossy().into_owned())]),
        )
        .unwrap(),
    ));
    let local = Arc::new(local);

    assert_eq!(
        request_to(local.clone(), "GET", "/api/config", None, "")
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = request_to(
        local.clone(),
        "POST",
        "/api/config/validate",
        Some("Bearer isolated-test-token"),
        r#"{"config":{"webui":{"officePreview":{"service":"invalid"}}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["valid"],
        false
    );
    let (status, body) = request_to(
        local.clone(),
        "POST",
        "/api/config/validate",
        Some("Bearer isolated-test-token"),
        r#"{"config":{"schemaVersion":1,"agent":{"model":"missing/model"},"model":{"providers":{"bad":{"protocol":"invalid","models":{}}}},"webui":{}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let invalid_core: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(invalid_core["valid"], false);
    assert!(!invalid_core["errors"].as_array().unwrap().is_empty());
    let (status, body) = request_to(
        local.clone(),
        "POST",
        "/api/config/validate",
        Some("Bearer isolated-test-token"),
        r#"{"raw":"schemaVersion: 1\nagent:\n  model: missing/model\nmodel:\n  providers: {}\nwebui: {}\n"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["valid"],
        false
    );
    assert_eq!(
        request_to(
            local.clone(),
            "POST",
            "/api/config/validate",
            Some("Bearer isolated-test-token"),
            r#"{"raw":"agent: ["}"#,
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let before = std::fs::read_to_string(root.join("pilotdeck.yaml")).unwrap();
    assert_eq!(
        request_to(
            local.clone(),
            "PUT",
            "/api/config",
            Some("Bearer isolated-test-token"),
            r#"{"config":{"webui":{"officePreview":{"service":"invalid"}}}}"#,
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        std::fs::read_to_string(root.join("pilotdeck.yaml")).unwrap(),
        before
    );
    let (status, _) = request_to(
        local.clone(),
        "PUT",
        "/api/config",
        Some("Bearer isolated-test-token"),
        r#"{"config":{"webui":{"runtime":{"serverPort":4300,"databasePath":"~/ui.db"},"officePreview":{"service":"LibreOffice","binaryPath":" /Applications/soffice "}},"customEnv":{"SERVER_PORT":"5500","EMPTY":" ","NUMBER":1}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = request_to(
        local.clone(),
        "GET",
        "/api/config",
        Some("Bearer isolated-test-token"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["config"]["webui"]["runtime"]["serverPort"], 4300);
    assert_eq!(
        value["config"]["webui"]["officePreview"]["service"],
        "libreoffice"
    );
    assert_eq!(
        value["config"]["webui"]["officePreview"]["binaryPath"],
        " /Applications/soffice "
    );
    assert_eq!(
        value["config"]["model"]["providers"]["parity"]["apiKey"],
        "<redacted>"
    );
    assert!(
        std::fs::read_to_string(root.join("pilotdeck.yaml"))
            .unwrap()
            .contains("# keep")
    );
    drop(local);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn local_routes_answer_without_any_upstream() {
    timeout(Duration::from_secs(3), async {
        assert_eq!(
            request("GET", "/health", None, "").await,
            (StatusCode::OK, r#"{"ok":true}"#.into())
        );
        assert_eq!(request("POST", "/health", None, "").await.0, StatusCode::OK);
        let (status, body) = request("GET", "/auth/local-token", None, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"],
            "isolated-test-token"
        );
        for path in ["/feishu/webhook", "/missing", "/ws"] {
            assert_eq!(
                request("GET", path, None, "").await,
                (StatusCode::NOT_FOUND, "not found".into())
            );
        }
        assert_eq!(
            request("GET", "/api/web/projects", None, "").await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                "GET",
                "/api/web/unknown",
                Some("bearer   isolated-test-token"),
                ""
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        for path in [
            "/api/module-host/describe",
            "/api/module-host/call",
            "/api/module-host/approvals/status",
            "/api/module-host/approvals/resume",
        ] {
            assert_eq!(
                request("POST", path, None, "{}").await.0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                request("GET", path, Some("Bearer isolated-test-token"), "")
                    .await
                    .0,
                StatusCode::METHOD_NOT_ALLOWED
            );
            let expected = if path.contains("approvals") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::NOT_IMPLEMENTED
            };
            assert_eq!(
                request("POST", path, Some("Bearer isolated-test-token"), "{}")
                    .await
                    .0,
                expected
            );
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn production_upgrade_ignores_initial_head_and_answers_later_ping_locally() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    timeout(Duration::from_secs(3), async {
        let (server_io, mut client) = tokio::io::duplex(4096);
        let job = tokio::spawn(server().serve_connection(server_io));
        let mut input = b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: key\r\n\r\n".to_vec();
        input.extend(crate::frame::client_frame(0x89, 4, b"head"));
        client.write_all(&input).await.unwrap();
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") { response.push(client.read_u8().await.unwrap()); }
        assert!(response.starts_with(b"HTTP/1.1 101"));
        client.write_all(&crate::frame::client_frame(0x89, 5, b"later")).await.unwrap();
        let mut pong = [0; 7];
        client.read_exact(&mut pong).await.unwrap();
        assert_eq!(pong.as_slice(), crate::frame::server_frame(10, b"later"));
        client.write_all(&crate::frame::client_frame(0x88, 0, b"")).await.unwrap();
        let mut tail = Vec::new(); client.read_to_end(&mut tail).await.unwrap();
        assert_eq!(tail, crate::frame::server_frame(8, &1000u16.to_be_bytes()));
        job.await.unwrap().unwrap();
    }).await.unwrap();
}
