use super::value::{deep_equal, field, object, set};
use super::*;
use pilotdeck_protocol::{parse_json, stringify_json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-sop-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\nmodules:\n  sop:\n    enabled: true\n    provider: staffdeck\n    endpoint: http://127.0.0.1:1\n    definitionsPath: sops.json\n    defaultSopId: sop\n").unwrap();
        Self(path)
    }
    fn sessions(&self) -> PathBuf {
        self.0.join("sop/sessions")
    }
    fn save(&self, session: &str, value: &JsValue) {
        fs::create_dir_all(self.sessions()).unwrap();
        fs::write(
            store::path(&self.sessions(), session),
            stringify_json(value).unwrap().unwrap() + "\n",
        )
        .unwrap();
    }
    fn bytes(&self, session: &str) -> Vec<u8> {
        fs::read(store::path(&self.sessions(), session)).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn js(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn state(session: &str, kind: &str) -> JsValue {
    let mut value = js(
        r#"{"schemaVersion":3,"sessionId":"session","bundle":{"sops":[{"id":"sop","skill_id":"sop","version":"original","content":{"nodes":[{"node_id":"approval","assignee_user_id":"approver"}]}}]},"state":{"active_skill_id":"sop","active_step_id":"approval","status":"handoff","slots_json":{"retained":true},"custom":{"keep":1}},"revision":2,"wait":{"id":"wait","kind":"handoff","skillId":"sop","stepId":"approval","createdAt":"2026-10-27T00:00:00.000Z"},"resumeRequests":{},"replyDelivery":{"turnId":"turn","phase":"pending","result":{"status":"handoff","replyFragment":"review","slotUpdates":{},"events":[]},"committedAt":"2026-10-27T00:00:00.000Z"},"updatedAt":"2026-10-27T00:00:00.000Z"}"#,
    );
    set(&mut value, "sessionId", session.into());
    let mut wait = field(&value, "wait").clone();
    set(&mut wait, "kind", kind.into());
    set(&mut value, "wait", wait);
    value
}
fn command(session: &str, source: &str) -> JsValue {
    object([
        ("sessionKey", session.into()),
        ("requestId", "reply".into()),
        ("waitId", "wait".into()),
        ("source", source.into()),
        ("message", "Reviewed".into()),
        ("expectedRevision", JsValue::Number(2.0)),
        ("slotUpdates", js(r#"{"a":1,"b":2}"#)),
    ])
}

#[tokio::test]
async fn raw_utf16_session_keys_keep_state_identity_and_wire_errors() {
    let root = Root::new();
    let raw = JsString::from_utf16(vec![0xd800]);
    let mut input = command("placeholder", "external_task");
    set(&mut input, "sessionKey", JsValue::String(raw.clone()));
    let raw_request = JsString::from_utf16(vec![0xdc00]);
    set(
        &mut input,
        "requestId",
        JsValue::String(raw_request.clone()),
    );
    assert!(value::nonempty(field(&input, "sessionKey")));
    assert_eq!(
        status_with_env(&root.0, &root.0, &input, &config::Env::new())
            .await
            .unwrap(),
        JsValue::Null
    );
    let error = resume_with_env(&root.0, &root.0, &input, &config::Env::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "SOP_SESSION_NOT_FOUND");
    let mut units: Vec<u16> = "No StaffDeck SOP state exists for '"
        .encode_utf16()
        .collect();
    units.extend_from_slice(raw.units());
    units.extend("'.".encode_utf16());
    assert_eq!(error.message, JsString::from_utf16(units));
    // Buffer.from(sessionId, "utf8") replacement encoding determines the file,
    // while the JSON sessionId comparison keeps the original UTF-16 identity.
    assert_eq!(
        store::path(&root.sessions(), raw.clone()),
        root.sessions().join("77-9.json")
    );
    let mut original = state("placeholder", "external_task");
    set(&mut original, "sessionId", JsValue::String(raw.clone()));
    fs::create_dir_all(root.sessions()).unwrap();
    fs::write(
        store::path(&root.sessions(), raw.clone()),
        stringify_json(&original).unwrap().unwrap(),
    )
    .unwrap();
    let status = status_with_env(&root.0, &root.0, &input, &config::Env::new())
        .await
        .unwrap();
    assert_eq!(field(&status, "sessionId"), &JsValue::String(raw.clone()));
    let first = resume_with_env(&root.0, &root.0, &input, &config::Env::new())
        .await
        .unwrap();
    assert_eq!(field(&first, "sessionId"), &JsValue::String(raw.clone()));
    assert_eq!(field(&first, "requestId"), &JsValue::String(raw_request));
    assert_eq!(field(&first, "duplicate"), &JsValue::Bool(false));
    let duplicate = resume_with_env(&root.0, &root.0, &input, &config::Env::new())
        .await
        .unwrap();
    assert_eq!(field(&duplicate, "duplicate"), &JsValue::Bool(true));
    set(&mut input, "sessionKey", "\u{fffd}".into());
    assert!(
        status_with_env(&root.0, &root.0, &input, &config::Env::new())
            .await
            .unwrap_err()
            .message
            .equals_str("StaffDeck SOP state for '\u{fffd}' has an invalid schema.")
    );
}

#[tokio::test]
async fn external_resume_migrates_preserves_snapshot_and_replays_without_writing() {
    let root = Root::new();
    for version in [1.0, 2.0, 3.0] {
        let session = format!("session/{version}");
        let mut original = state(&session, "external_task");
        set(&mut original, "schemaVersion", JsValue::Number(version));
        root.save(&session, &original);
        let input = command(&session, "external_task");
        let snapshot = status_with_env(&root.0, &root.0, &input, &config::Env::new())
            .await
            .unwrap();
        assert_eq!(
            text(field(&snapshot, "wait"), "kind").as_deref(),
            Some("external_task")
        );
        let result = resume_with_env(&root.0, &root.0, &input, &config::Env::new())
            .await
            .unwrap();
        assert_eq!(field(&result, "revision").as_number(), Some(3.0));
        let saved = store::read(&root.sessions(), session.as_str())
            .unwrap()
            .unwrap();
        assert!(field(&saved, "wait").is_undefined());
        assert_eq!(
            text(field(&saved, "state"), "status").as_deref(),
            Some("active")
        );
        assert_eq!(
            field(field(field(&saved, "state"), "slots_json"), "retained").as_bool(),
            Some(true)
        );
        assert_eq!(field(&saved, "bundle"), field(&original, "bundle"));
        assert_eq!(
            field(&saved, "replyDelivery"),
            field(&original, "replyDelivery")
        );
        assert_eq!(
            field(field(&saved, "state"), "custom"),
            field(field(&original, "state"), "custom")
        );
        let before = root.bytes(&session);
        let mut reordered = input.clone();
        set(&mut reordered, "slotUpdates", js(r#"{"b":2,"a":1}"#));
        let duplicate = resume_with_env(&root.0, &root.0, &reordered, &config::Env::new())
            .await
            .unwrap();
        assert_eq!(field(&duplicate, "duplicate").as_bool(), Some(true));
        assert_eq!(root.bytes(&session), before);
        set(&mut reordered, "message", "Different".into());
        assert_eq!(
            resume_with_env(&root.0, &root.0, &reordered, &config::Env::new())
                .await
                .unwrap_err()
                .code,
            "SOP_RESUME_REQUEST_CONFLICT"
        );
        assert_eq!(root.bytes(&session), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store::path(&root.sessions(), session.as_str()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn competing_resumes_share_one_disk_transition() {
    let root = Root::new();
    root.save("session", &state("session", "external_task"));
    let input = command("session", "external_task");
    let sessions = root.sessions();
    let (a, b) = tokio::join!(
        store::resume(&sessions, "session", &input, None),
        store::resume(&sessions, "session", &input, None)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(field(&a, "duplicate"), field(&b, "duplicate"));
    assert_eq!(field(&a, "revision").as_number(), Some(3.0));
    assert_eq!(field(&b, "revision").as_number(), Some(3.0));
    assert_eq!(
        field(
            &store::read(&root.sessions(), "session").unwrap().unwrap(),
            "resumeRequests"
        )
        .as_object()
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn stale_wait_source_corrupt_state_and_missing_session_never_write() {
    let root = Root::new();
    root.save("session", &state("session", "external_task"));
    let before = root.bytes("session");
    for (key, value, code) in [
        ("waitId", "stale".into(), "SOP_WAIT_STALE"),
        ("source", "invalid".into(), "SOP_RESUME_SOURCE_INVALID"),
        (
            "expectedRevision",
            JsValue::Number(1.0),
            "SOP_REVISION_CONFLICT",
        ),
    ] {
        let mut input = command("session", "external_task");
        set(&mut input, key, value);
        assert_eq!(
            store::resume(&root.sessions(), "session", &input, None)
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(root.bytes("session"), before);
    }
    assert_eq!(
        store::resume(
            &root.sessions(),
            "missing",
            &command("missing", "external_task"),
            None
        )
        .await
        .unwrap_err()
        .code,
        "SOP_SESSION_NOT_FOUND"
    );
    fs::write(store::path(&root.sessions(), "session"), "{broken").unwrap();
    assert!(store::status(&root.sessions(), "session").await.is_err());
    assert!(
        store::resume(
            &root.sessions(),
            "session",
            &command("session", "external_task"),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(root.bytes("session"), b"{broken");
    assert_ne!(
        store::path(&root.sessions(), "a/b"),
        store::path(&root.sessions(), "a-b")
    );
}

struct AuthorityServer {
    origin: String,
    revoked: Arc<AtomicBool>,
    approver_calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for AuthorityServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl AuthorityServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let revoked = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let (revocation, approver_calls) = (revoked.clone(), calls.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut bytes = Vec::new();
                loop {
                    let mut block = [0; 2048];
                    let n = socket.read(&mut block).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&block[..n]);
                    if bytes.windows(4).any(|s| s == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                let path = request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default();
                let bearer = request
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("authorization")
                            .then_some(value.trim())
                    })
                    .unwrap_or_default();
                let (status, body) = match (path, bearer) {
                    ("/api/auth/me", "Bearer test-approver") => {
                        approver_calls.fetch_add(1, Ordering::SeqCst);
                        (
                            200,
                            serde_json::json!({"id":"approver","tenant_id":"tenant","source":"web","role":"member","disabled":false}),
                        )
                    }
                    ("/api/auth/me", "Bearer test-wrong") => (
                        200,
                        serde_json::json!({"id":"other","tenant_id":"tenant","source":"web","role":"member"}),
                    ),
                    ("/api/auth/me", "Bearer test-actor") => (
                        200,
                        serde_json::json!({"id":"actor","tenant_id":"tenant","source":"web","disabled":false}),
                    ),
                    ("/api/auth/me/api-credentials", "Bearer test-actor") => (
                        200,
                        serde_json::json!([{"id":"credential","user_id":"actor","status":if revocation.load(Ordering::SeqCst) { "revoked" } else { "active" },"access":"user_full_access","key_prefix":"test-only-prefix-000…","expires_at":"2099-01-01T00:00:00Z"}]),
                    ),
                    ("/api/v1/agents/agent", "Bearer test-only-prefix-000-full") => (
                        200,
                        serde_json::json!({"id":"agent","status":"active","is_overall":false}),
                    ),
                    _ => (401, serde_json::json!({"error":"rejected"})),
                };
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            origin,
            revoked,
            approver_calls: calls,
            task,
        }
    }
    fn env(&self) -> config::Env {
        config::Env::from([
            ("STAFFDECK_FORMAL_API_ORIGIN".into(), self.origin.clone()),
            ("STAFFDECK_APPROVAL_USER_ID".into(), "approver".into()),
            ("STAFFDECK_COPY_TENANT_ID".into(), "tenant".into()),
            ("STAFFDECK_COPY_TARGET_AGENT_ID".into(), "agent".into()),
            ("STAFFDECK_COPY_PILOTDECK_USER_ID".into(), "owner".into()),
            ("STAFFDECK_COPY_ACTOR_USER_ID".into(), "actor".into()),
            (
                "STAFFDECK_SOP_MANAGEMENT_CREDENTIAL_ID".into(),
                "credential".into(),
            ),
            ("STAFFDECK_COPY_USER_TOKEN".into(), "test-actor".into()),
            (
                "STAFFDECK_SOP_MANAGEMENT_API_KEY".into(),
                "test-only-prefix-000-full".into(),
            ),
        ])
    }
}
fn admit(root: &Root, server: &AuthorityServer) {
    let database = rusqlite::Connection::open(root.0.join("auth.db")).unwrap();
    database
        .execute_batch(
            "CREATE TABLE users(id TEXT, is_active INTEGER); INSERT INTO users VALUES('owner',1);",
        )
        .unwrap();
    let project = root.0.to_string_lossy();
    let chats = crate::session_chat_dir(&root.0, &root.0, &project);
    fs::create_dir_all(&chats).unwrap();
    let entry = object([
        ("type", "session_metadata".into()),
        ("sessionId", "session".into()),
        ("turnId", "host-admission".into()),
        ("sequence", JsValue::Number(1.0)),
        ("createdAt", "2026-10-27T00:00:00.000Z".into()),
        (
            "metadata",
            object([(
                "staffDeckAdmission",
                object([
                    ("tenantId", "tenant".into()),
                    ("agentId", "agent".into()),
                    ("pilotDeckUserId", "owner".into()),
                    ("projectKey", project.as_ref().into()),
                    ("actorUserId", "actor".into()),
                    ("credentialId", "credential".into()),
                    ("staffDeckOrigin", server.origin.as_str().into()),
                ]),
            )]),
        ),
    ]);
    fs::write(
        chats.join("session.jsonl"),
        stringify_json(&entry).unwrap().unwrap() + "\n",
    )
    .unwrap();
}

#[tokio::test]
async fn human_approval_requires_the_original_catalog_session() {
    let root = Root::new();
    let server = AuthorityServer::start().await;
    admit(&root, &server);
    let env = server.env();
    root.save("session", &state("session", "handoff"));
    let before = root.bytes("session");
    let mut input = command("session", "human");
    set(
        &mut input,
        "approverAuthorization",
        "Bearer test-approver".into(),
    );
    assert_eq!(
        status_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "SOP_APPROVAL_SESSION_FORBIDDEN"
    );
    assert_eq!(
        resume_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "SOP_APPROVAL_SESSION_FORBIDDEN"
    );
    assert_eq!(root.bytes("session"), before);
}

#[tokio::test]
async fn human_http_identity_original_binding_receipt_and_revocation() {
    let root = Root::new();
    let server = AuthorityServer::start().await;
    admit(&root, &server);
    let chats = crate::session_chat_dir(&root.0, &root.0, root.0.to_str().unwrap());
    let transcript = chats.join("session.jsonl");
    let admission_only = fs::read(&transcript).unwrap();
    let mut entry = parse_json(String::from_utf8_lossy(&admission_only).trim()).unwrap();
    let mut metadata = field(&entry, "metadata").clone();
    set(&mut metadata, "title", "Original session".into());
    set(&mut entry, "metadata", metadata);
    let catalog_visible = stringify_json(&entry).unwrap().unwrap() + "\n";
    fs::write(&transcript, &catalog_visible).unwrap();
    let env = server.env();
    root.save("session", &state("session", "handoff"));
    let mut input = command("session", "human");
    assert_eq!(
        status_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "APPROVAL_AUTH_REQUIRED"
    );
    let before = root.bytes("session");
    set(
        &mut input,
        "approverAuthorization",
        "Bearer test-wrong".into(),
    );
    assert_eq!(
        resume_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "APPROVAL_SUBJECT_MISMATCH"
    );
    assert_eq!(root.bytes("session"), before);
    set(
        &mut input,
        "approverAuthorization",
        "Bearer test-approver".into(),
    );
    let snapshot = status_with_env(&root.0, &root.0, &input, &env)
        .await
        .unwrap();
    assert_eq!(
        text(field(&snapshot, "approval"), "version").as_deref(),
        Some("original")
    );
    let result = resume_with_env(&root.0, &root.0, &input, &env)
        .await
        .unwrap();
    assert_eq!(field(&result, "duplicate").as_bool(), Some(false));
    let saved = root.bytes("session");
    assert!(!String::from_utf8_lossy(&saved).contains("Bearer"));
    assert!(!String::from_utf8_lossy(&saved).contains("test-approver"));
    let duplicate = resume_with_env(&root.0, &root.0, &input, &env)
        .await
        .unwrap();
    assert_eq!(field(&duplicate, "duplicate").as_bool(), Some(true));
    assert_eq!(root.bytes("session"), saved);
    assert_eq!(server.approver_calls.load(Ordering::SeqCst), 3);
    fs::write(&transcript, &admission_only).unwrap();
    assert_eq!(
        resume_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "SOP_APPROVAL_SESSION_FORBIDDEN"
    );
    assert_eq!(root.bytes("session"), saved);
    fs::write(&transcript, &catalog_visible).unwrap();
    let corrupt_catalog = chats.join("invalid.jsonl");
    fs::write(
        &corrupt_catalog,
        r#"{"type":"session_metadata","metadata":{"title":"bad\q"}}"#,
    )
    .unwrap();
    assert_eq!(
        status_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "gateway_request_failed"
    );
    assert_eq!(
        resume_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "SOP_RESUME_FAILED"
    );
    assert_eq!(root.bytes("session"), saved);
    fs::remove_file(corrupt_catalog).unwrap();
    server.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        resume_with_env(&root.0, &root.0, &input, &env)
            .await
            .unwrap_err()
            .code,
        "SOP_APPROVAL_SESSION_FORBIDDEN"
    );
    assert_eq!(root.bytes("session"), saved);
}

#[test]
fn date_and_deep_receipt_checks() {
    for ms in [0, 951_782_400_000, 1_791_547_200_123, -1] {
        assert_eq!(value::expiry_ms(&value::iso_timestamp(ms)), Some(ms));
    }
    assert_eq!(value::expiry_ms("1970-01-01T08:00:00+08:00"), Some(0));
    assert_eq!(value::expiry_ms("2026-02-30T00:00:00Z"), None);
    assert!(deep_equal(
        &js(r#"{"a":1,"b":[2]}"#),
        &js(r#"{"b":[2],"a":1}"#)
    ));
    assert!(!deep_equal(&JsValue::Number(0.0), &JsValue::Number(-0.0)));
}
