use super::super::native_http::{JsonFuture, JsonResponse};
use super::*;
use std::sync::Mutex;
type HttpCalls = Vec<(String, String, Option<String>)>;
struct MockHttp {
    body: JsValue,
    status: u16,
    calls: Mutex<HttpCalls>,
}
impl JsonTransport for MockHttp {
    fn request(&self, input: JsonRequest) -> JsonFuture<'_> {
        self.calls
            .lock()
            .unwrap()
            .push((input.method.into(), input.url.to_string(), input.bearer));
        let response = JsonResponse {
            body: self.body.clone(),
            status: self.status,
        };
        Box::pin(async move { Ok(response) })
    }
}
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&UNDEFINED)
}
#[tokio::test]
async fn fixed_approver_matches_ten_real_ts_authentication_cells() {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/native-sop/oracle.json"
    ))
    .unwrap();
    let JsValue::Array(rows) = field(&oracle, "auth") else {
        panic!("auth rows")
    };
    assert_eq!(rows.len(), 10);
    for row in rows {
        let name = field(row, "name").as_string().unwrap().to_utf8().unwrap();
        let http = Arc::new(MockHttp {
            body: field(row, "user").clone(),
            status: row
                .get("status")
                .and_then(JsValue::as_number)
                .unwrap_or(200.0) as u16,
            calls: Mutex::new(vec![]),
        });
        let authority = FixedApprovalAuthenticator::new(
            "http://127.0.0.1:1/nested/path",
            "tenant".into(),
            "approver".into(),
            http.clone(),
        )
        .unwrap();
        let bearer = row
            .get("bearer")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "fixture-bearer".into());
        let result = authority.authenticate(&bearer).await;
        let expected = field(row, "outcome");
        assert_eq!(
            result.is_ok(),
            field(expected, "ok") == &JsValue::Bool(true),
            "{name}"
        );
        match result {
            Ok(subject) => {
                let actual = crate::metadata::object([
                    ("tenantId", JsValue::String(subject.tenant_id)),
                    ("userId", JsValue::String(subject.user_id)),
                    ("source", JsValue::String(subject.source)),
                    ("role", JsValue::String(subject.role)),
                    ("disabled", JsValue::Bool(subject.disabled)),
                ]);
                assert_eq!(
                    stringify_json(&actual).unwrap(),
                    stringify_json(field(expected, "result")).unwrap(),
                    "{name}"
                );
            }
            Err(error) => {
                if let Some(code) = expected.get("code") {
                    assert_eq!(error.code.as_ref(), Some(code), "{name}");
                } else {
                    assert_eq!(
                        error.code,
                        Some("PUBLIC_APPROVAL_BRIDGE_FAILED".into()),
                        "{name}: ordinary authority exception uses bridge failure"
                    );
                }
                if let Some(status) = expected.get("status").and_then(JsValue::as_number) {
                    assert_eq!(error.status, Some(status as u16), "{name}");
                }
            }
        }
        let calls = http.calls.lock().unwrap();
        let JsValue::Array(wire) = field(row, "requests") else {
            panic!("requests")
        };
        assert_eq!(calls.len(), wire.len(), "{name}");
        if let Some((method, url, token)) = calls.first() {
            assert_eq!(method, "GET");
            assert_eq!(url, "http://127.0.0.1:1/api/auth/me");
            assert_eq!(token.as_deref(), Some("fixture-bearer"));
        }
    }
}
#[test]
fn fixed_configuration_rejects_foreign_protocol_credentials_and_empty_binding() {
    let http = Arc::new(MockHttp {
        body: JsValue::Null,
        status: 200,
        calls: Mutex::new(vec![]),
    });
    for origin in [
        "file:///fixture",
        "https://user:password@example.invalid/",
        "https://example.invalid/?key=secret",
        "https://example.invalid/#fragment",
    ] {
        let result = FixedApprovalAuthenticator::new(origin, "t".into(), "u".into(), http.clone());
        assert!(matches!(
            result,
            Err(ApprovalFailure {
                status: Some(400),
                ..
            })
        ));
    }
    assert!(
        FixedApprovalAuthenticator::new("https://example.invalid/", " ".into(), "u".into(), http)
            .is_err()
    );
}
#[test]
fn readonly_installation_owner_reads_live_wal_and_never_creates_missing_database() {
    let root = std::env::temp_dir().join(format!("pdrs-sop-owner-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("auth.db");
    assert!(read_owner_sqlite(&path).is_none());
    assert!(!path.exists());
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE users(id TEXT, is_active INTEGER); INSERT INTO users VALUES ('fixture-owner',1);").unwrap();
    assert!(PathBuf::from(format!("{}-wal", path.display())).exists());
    assert_eq!(read_owner_sqlite(&path), Some("fixture-owner".into()));
    writer
        .execute("INSERT INTO users VALUES ('second-owner',1)", [])
        .unwrap();
    assert!(read_owner_sqlite(&path).is_none());
    writer.execute("UPDATE users SET is_active=0", []).unwrap();
    assert!(read_owner_sqlite(&path).is_none());
    drop(writer);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn original_admission_requires_consistent_native_session_metadata() {
    let session: JsString = "s1".into();
    let admission=parse_json(r#"{"tenantId":"t","agentId":"a","pilotDeckUserId":"u","projectKey":"/owned/project","actorUserId":"actor","credentialId":"credential","staffDeckOrigin":"http://127.0.0.1:1"}"#).unwrap();
    let entry = |turn: &str, id: &str, value: JsValue| {
        crate::metadata::object([
            ("type", "session_metadata".into()),
            ("sessionId", id.into()),
            ("turnId", turn.into()),
            (
                "metadata",
                crate::metadata::object([("staffDeckAdmission", value)]),
            ),
        ])
    };
    assert_eq!(
        read_admission(
            &[entry("host-admission", "s1", admission.clone())],
            &session
        ),
        Some(admission.clone())
    );
    assert!(read_admission(&[entry("foreign-turn", "s1", admission.clone())], &session).is_none());
    assert!(
        read_admission(
            &[entry("host-admission", "other", admission.clone())],
            &session
        )
        .is_none()
    );
    let mut changed = admission.clone();
    if let JsValue::Object(ref mut object) = changed {
        object.insert("credentialId".into(), "other".into());
    }
    assert!(
        read_admission(
            &[
                entry("host-admission", "s1", admission),
                entry("host-admission", "s1", changed)
            ],
            &session
        )
        .is_none()
    );
}

struct EmptyCatalog;
impl NativeApprovalCatalog for EmptyCatalog {
    fn projects(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<JsString>, ApprovalFailure>> + Send + '_>> {
        Box::pin(async { panic!("catalog cannot grant access before original installation owner") })
    }
    fn has_session<'a>(
        &'a self,
        _: &'a JsString,
        _: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async { Ok(false) })
    }
    fn admission<'a>(
        &'a self,
        _: &'a JsString,
        _: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }
}
#[tokio::test]
async fn missing_owner_helper_is_fail_closed_before_catalog_authority() {
    let resolver = NativeSessionResolver {
        database: PathBuf::from("/does-not-exist/auth.db"),
        helper: PathBuf::from("/does-not-exist/bundle-helper"),
        catalog: Arc::new(EmptyCatalog),
    };
    let binding = ApprovalBinding {
        tenant_id: "t".into(),
        agent_id: "a".into(),
        pilot_deck_user_id: "u".into(),
    };
    let subject = ApprovalSubject {
        tenant_id: "t".into(),
        source: "web".into(),
        disabled: false,
        user_id: "approver".into(),
        role: "member".into(),
    };
    assert!(
        resolver
            .resolve(&"s1".into(), None, &binding, &subject)
            .await
            .is_none()
    );
}

struct AdmissionHttp {
    requests: Mutex<Vec<String>>,
    revoked: bool,
}
impl JsonTransport for AdmissionHttp {
    fn request(&self, input: JsonRequest) -> JsonFuture<'_> {
        self.requests.lock().unwrap().push(input.url.path().into());
        let body = match input.url.path() {
            "/api/auth/me" => {
                parse_json(r#"{"id":"actor","tenant_id":"tenant","source":"web","disabled":false}"#)
                    .unwrap()
            }
            "/api/auth/me/api-credentials" => crate::metadata::object([
                ("id", "credential".into()),
                ("user_id", "actor".into()),
                ("status", "active".into()),
                ("access", "user_full_access".into()),
                ("key_prefix", "12345678901234567890…".into()),
                (
                    "revoked_at",
                    if self.revoked {
                        "revoked".into()
                    } else {
                        JsValue::Null
                    },
                ),
                ("expires_at", "2099-01-01T00:00:00.000Z".into()),
            ]),
            "/api/v1/agents/agent" => {
                parse_json(r#"{"id":"agent","status":"active","is_overall":false}"#).unwrap()
            }
            other => panic!("unexpected identity route {other}"),
        };
        let body = if input.url.path() == "/api/auth/me/api-credentials" {
            JsValue::Array(vec![body])
        } else {
            body
        };
        Box::pin(async move { Ok(JsonResponse { status: 200, body }) })
    }
}
#[tokio::test]
async fn native_admission_verifies_actor_complete_management_key_and_live_target() {
    let identity = AdmissionIdentity {
        origin: "http://127.0.0.1:1".into(),
        tenant: "tenant".into(),
        agent: "agent".into(),
        actor: "actor".into(),
        credential: "credential".into(),
        actor_bearer: "fixture-user-bearer".into(),
        discovery_endpoint: "http://127.0.0.1:1/api/v1".into(),
        discovery_agent: "agent".into(),
        discovery_key: "12345678901234567890-full-fixture-key".into(),
        pilot_user: "pilot-user".into(),
        expected_pilot_user: "pilot-user".into(),
    };
    let http = AdmissionHttp {
        requests: Mutex::new(vec![]),
        revoked: false,
    };
    let actual = authenticate_admission(&identity, &"/owned/project".into(), &http, 0.0)
        .await
        .unwrap();
    assert_eq!(actual.get("actorUserId"), Some(&JsValue::from("actor")));
    assert_eq!(
        actual.get("credentialId"),
        Some(&JsValue::from("credential"))
    );
    assert_eq!(
        actual.get("projectKey"),
        Some(&JsValue::from("/owned/project"))
    );
    assert_eq!(
        *http.requests.lock().unwrap(),
        [
            "/api/auth/me",
            "/api/auth/me/api-credentials",
            "/api/v1/agents/agent"
        ]
    );
    let revoked = AdmissionHttp {
        requests: Mutex::new(vec![]),
        revoked: true,
    };
    assert!(
        authenticate_admission(&identity, &"/owned/project".into(), &revoked, 0.0)
            .await
            .is_none()
    );
    assert_eq!(
        revoked.requests.lock().unwrap().len(),
        2,
        "revoked key never grants target access"
    );
}
#[tokio::test]
async fn production_auth_http_rejects_before_reading_an_error_body() {
    use super::super::native_http::HyperJsonTransport;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = vec![0; 4096];
        let size = socket.read(&mut raw).await.unwrap();
        assert!(
            String::from_utf8_lossy(&raw[..size]).contains("authorization: Bearer fixture-bearer")
        );
        socket
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let http = Arc::new(HyperJsonTransport::default());
    let auth =
        FixedApprovalAuthenticator::new(&format!("http://{address}"), "t".into(), "u".into(), http)
            .unwrap();
    let answer = tokio::time::timeout(
        Duration::from_secs(2),
        auth.authenticate(&"fixture-bearer".into()),
    )
    .await
    .expect("401 does not await untrusted body");
    assert!(matches!(
        answer,
        Err(ApprovalFailure {
            status: Some(401),
            ..
        })
    ));
    server.abort();
    let _ = server.await;
}
