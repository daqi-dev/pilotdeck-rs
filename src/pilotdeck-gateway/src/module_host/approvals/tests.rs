//! #1350: byte-level replay of the module-host approvals golden fixture
//! (every cell one test), the raw-byte pins the replay normalization cannot
//! express, the shared-store visibility the A8.4 requirement names (an HTTP
//! resume is the state the WS `sop_status`/`sop_resume` family reads), and
//! the transport-level pins the store cannot reach. The fixture was recorded
//! against the real TS stack (`startGatewayServer` + the real
//! `publicApprovalHttpTransport` + the real `publicApprovalBridge` + the real
//! `StaffDeckSopControlPlane`/`SopStateStore` over seeded state files, plus a
//! scripted stub bridge and a scripted dynamic binding); here the same
//! requests go through the native hyper listener with the approvals transport
//! composed in front of the #1349 module-host transport and the #1209 router,
//! which serves the fallthrough 404s.
use super::pending::{PendingSopStore, deep_strict_equal};
use super::*;
use crate::{http_proxy, native_http, test_baseline};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const ORACLE: &str = include_str!("../../../tests/fixtures/module-host-approvals/oracle.json");
const DEADLINE: Duration = Duration::from_secs(10);
/// The recorder's quiet window: a response is complete after a quarter second
/// of silence while the connection stays open.
const QUIET: Duration = Duration::from_millis(250);
const TOKEN: &str = "fixture-token";

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1350 oracle JSON")
}

struct Cell {
    id: String,
    server: String,
    request: Vec<u8>,
    response: String,
    closed: bool,
    half_close: bool,
}

fn cell(id: &str) -> Cell {
    let value = oracle();
    let cells = value["cells"].as_array().expect("#1350 oracle cells array");
    for entry in cells {
        if entry["id"].as_str() == Some(id) {
            let mut request = entry["request"]
                .as_str()
                .expect("#1350 oracle cell request")
                .to_owned();
            // The budget cells carry the pad marker plus a size; the replay
            // writes the real 'a' padding the recorder wrote.
            if let Some(pad) = entry.get("pad").filter(|pad| !pad.is_null()) {
                let marker = pad["marker"].as_str().expect("#1350 oracle pad marker");
                let size = pad["size"].as_u64().expect("#1350 oracle pad size") as usize;
                assert!(
                    request.contains(marker),
                    "#1350 cell {id} pad marker missing from its request"
                );
                request = request.replace(marker, &"a".repeat(size));
            }
            return Cell {
                id: id.to_owned(),
                server: entry["server"]
                    .as_str()
                    .expect("#1350 oracle cell server")
                    .to_owned(),
                request: request.into_bytes(),
                response: entry["response"]
                    .as_str()
                    .expect("#1350 oracle cell response")
                    .to_owned(),
                closed: entry["closed"]
                    .as_bool()
                    .expect("#1350 oracle cell closed flag"),
                half_close: entry["halfClose"].as_bool().unwrap_or(false),
            };
        }
    }
    panic!("#1350 cell {id} is missing from the committed oracle");
}

// ------------------------------------------------------- fixture collaborators

fn json(text: &str) -> JsValue {
    parse_json(text).expect("#1350 fixture JSON must parse")
}

fn binding(tenant: &str, agent: &str, user: &str) -> ApprovalBinding {
    ApprovalBinding {
        tenant_id: JsString::from(tenant),
        agent_id: JsString::from(agent),
        pilot_deck_user_id: JsString::from(user),
    }
}

/// The scripted authenticate port, the Rust mirror of the fixture's subject
/// table: one subject per bearer, unknown bearers the formal authority's own
/// rejection.
struct FixtureAuthority;

fn subject(name: &str) -> Option<ApprovalSubject> {
    let row = match name {
        "staff-admin" => ("t1", "web", false, "staff-admin-id", "admin"),
        "staff-member" => ("t1", "web", false, "staff-member-id", "member"),
        "staff-other" => ("t1", "web", false, "someone-else", "member"),
        "staff-tenant2" => ("t2", "web", false, "staff-admin-id", "admin"),
        "staff-disabled" => ("t1", "web", true, "staff-admin-id", "admin"),
        "staff-source" => ("t1", "cli", false, "staff-admin-id", "admin"),
        "staff-guest" => ("t1", "web", false, "staff-admin-id", "guest"),
        "staff-emptyuser" => ("t1", "web", false, "", "admin"),
        _ => return None,
    };
    Some(ApprovalSubject {
        tenant_id: JsString::from(row.0),
        source: JsString::from(row.1),
        disabled: row.2,
        user_id: JsString::from(row.3),
        role: JsString::from(row.4),
    })
}

impl ApprovalAuthenticator for FixtureAuthority {
    fn authenticate<'a>(
        &'a self,
        bearer: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<ApprovalSubject, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            match subject(&bearer.to_utf8().unwrap_or_default()) {
                Some(subject) => Ok(subject),
                None => Err(failure(401, "APPROVAL_AUTH_REJECTED")),
            }
        })
    }
}

/// The scripted resolveSession port, the Rust mirror of the fixture's
/// registered session catalog.
struct Catalog;

fn session_project(session: &str) -> Option<&'static str> {
    match session {
        "sess-proj2" => Some("proj2"),
        "sess-handoff" | "sess-handoff2" | "sess-handoff3" | "sess-handoff4" | "sess-active"
        | "sess-ext" | "sess-waitstale" | "sess-rev" | "sess-badpin" | "sess-unassigned"
        | "sess-gone" | "sess-done" | "sess-mismatch" | "sess-dyn2" | "sess-dynfail" => {
            Some("proj1")
        }
        _ => None,
    }
}

impl ApprovalSessionResolver for Catalog {
    fn resolve<'a>(
        &'a self,
        session_key: &'a JsString,
        _project_key: Option<&'a JsValue>,
        resolved: &'a ApprovalBinding,
        _subject: &'a ApprovalSubject,
    ) -> Pin<Box<dyn Future<Output = Option<ApprovalSession>> + Send + 'a>> {
        Box::pin(async move {
            let key = session_key.to_utf8().unwrap_or_default();
            let project = session_project(&key)?;
            let session_key = if key == "sess-mismatch" {
                JsString::from("other-key")
            } else {
                session_key.clone()
            };
            Some(ApprovalSession {
                tenant_id: resolved.tenant_id.clone(),
                agent_id: resolved.agent_id.clone(),
                pilot_deck_user_id: resolved.pilot_deck_user_id.clone(),
                session_key,
                project_key: Some(JsValue::String(JsString::from(project))),
            })
        })
    }
}

/// The scripted resolveBinding port of the fixture's `dynamic` server.
struct DynamicResolver;

impl ApprovalBindingResolver for DynamicResolver {
    fn resolve<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<ApprovalBinding, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            let key = input
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .map(|key| key.to_utf8().unwrap_or_default())
                .unwrap_or_default();
            match key.as_str() {
                "sess-dyn2" => Ok(binding("t2", "a2", "u2")),
                "sess-dynfail" => Err(ApprovalFailure {
                    status: Some(403),
                    code: None,
                }),
                _ => Ok(binding("t1", "a1", "u1")),
            }
        })
    }
}

/// The scripted stub bridge, the Rust mirror of the fixture's plain object.
struct StubBridge(ApprovalBinding);

impl ApprovalBridge for StubBridge {
    fn binding(&self) -> &ApprovalBinding {
        &self.0
    }

    fn dynamic_binding(&self) -> bool {
        false
    }

    fn status<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            let session = input
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .map(|key| key.to_utf8().unwrap_or_default())
                .unwrap_or_default();
            match session.as_str() {
                "stub-snapshot" => Ok(json(
                    r#"{"sessionId":"stub-snapshot","revision":7,"state":{}}"#,
                )),
                "stub-null" => Ok(JsValue::Null),
                "stub-undef" => Ok(JsValue::Undefined),
                "stub-echo" => Ok(crate::metadata::object([
                    (
                        "auth",
                        input
                            .get("approverAuthorization")
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    ),
                    (
                        "tenant",
                        input.get("tenantId").cloned().unwrap_or(JsValue::Undefined),
                    ),
                ])),
                "stub-fail-status" => Err(failure(400, "X_CUSTOM")),
                "stub-fail-teapot" => Err(ApprovalFailure {
                    status: Some(418),
                    code: Some(JsValue::Number(123.0)),
                }),
                "stub-fail-unknown" => Err(failure(499, "X_499")),
                "stub-fail-plain" => Err(ApprovalFailure {
                    status: None,
                    code: None,
                }),
                "stub-fail-codeonly" => Err(ApprovalFailure {
                    status: None,
                    code: Some(JsValue::String(JsString::from("SOME_CODE"))),
                }),
                _ => Ok(json(r#"{"stub":"default"}"#)),
            }
        })
    }

    fn resume<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            let session = input
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .map(|key| key.to_utf8().unwrap_or_default())
                .unwrap_or_default();
            match session.as_str() {
                "stub-resume-ok" => Ok(json(r#"{"accepted":true,"marker":"ok"}"#)),
                "stub-resume-undef" => Ok(JsValue::Undefined),
                "stub-resume-conflict" => Err(ApprovalFailure {
                    status: None,
                    code: Some(JsValue::String(JsString::from(
                        "SOP_RESUME_REQUEST_CONFLICT",
                    ))),
                }),
                "stub-resume-413" => Err(failure(413, "X_BRIDGE")),
                _ => Ok(json(r#"{"stub":"resumed"}"#)),
            }
        })
    }
}

// -------------------------------------------------------------- seeded store

/// The seeded persisted states, the same bytes the recorder wrote under the
/// profile's stateRoot (the store's own base64url naming is the TS file
/// concern this port keeps out of scope).
const HANDOFF_WAIT: &str = r#"{"id":"wait-1","kind":"handoff","skillId":"skill-1","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}"#;
const HANDOFF_STATE: &str = r#"{"version":1,"selected_skill_id":"skill-1","slots_json":{},"skill_stack_json":[],"successful_tool_names":[],"status":"handoff","active_skill_id":"skill-1","active_step_id":"step-1","awaiting_input_json":{"wait_id":"wait-1"}}"#;
const BUNDLE: &str = r#"{"sops":[{"id":"sop-1","skill_id":"skill-1","version":"1.0.0","content":{"nodes":[{"node_id":"step-1","assignee_user_id":"staff-member-id"}]}}]}"#;
const BUNDLE_UNASSIGNED: &str = r#"{"sops":[{"id":"sop-1","skill_id":"skill-1","version":"1.0.0","content":{"nodes":[{"node_id":"step-1","assignee_user_id":"  "}]}}]}"#;
const ACTIVE_STATE: &str = r#"{"version":1,"selected_skill_id":"skill-1","slots_json":{"existing":"1"},"skill_stack_json":[],"successful_tool_names":[],"status":"active"}"#;
const EXT_WAIT: &str = r#"{"id":"wait-ext","kind":"external_task","skillId":"skill-1","stepId":"step-2","createdAt":"2026-01-01T00:00:00.000Z"}"#;
const EXT_STATE: &str = r#"{"version":1,"selected_skill_id":"skill-1","slots_json":{},"skill_stack_json":[],"successful_tool_names":[],"status":"waiting_external_task","active_skill_id":"skill-1","active_step_id":"step-2","awaiting_input_json":{"wait_id":"wait-ext"}}"#;
const STALE_WAIT: &str = r#"{"id":"wait-old","kind":"handoff","skillId":"skill-1","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}"#;
const BADPIN_WAIT: &str = r#"{"id":"wait-1","kind":"handoff","skillId":"skill-x","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}"#;

/// One fresh store with the fixture's seeds, the way a state root loads: the
/// raw persisted shape per session, plus the already-resumed `sess-done`
/// receipt (pre-resumed through the store's own resume, the WS lane's call
/// shape, exactly like the recorder).
/// The fixture profile resolver: the unkeyed default and `proj1` resolve to
/// one root, everything else has no profile.
fn root1_resolver() -> super::pending::ProfileResolver {
    Arc::new(|project_key: Option<&JsValue>| match project_key {
        None => Some("root1".to_owned()),
        Some(JsValue::String(key)) if key.equals_str("proj1") => Some("root1".to_owned()),
        _ => None,
    })
}

/// One persisted state over a store, the raw shape a state file would load
/// (revision, optional wait, state, bundle).
fn seed_persisted(
    store: &Arc<PendingSopStore>,
    session: &str,
    revision: i64,
    wait: Option<&str>,
    state: &str,
    bundle: &str,
) {
    let persisted = format!(
        r#"{{"schemaVersion":3,"sessionId":{session:?},"bundle":{bundle},"state":{state},"revision":{revision}{},"resumeRequests":{{}},"updatedAt":"2026-01-01T00:00:00.000Z"}}"#,
        wait.map(|wait| format!(r#","wait":{wait}"#))
            .unwrap_or_default()
    );
    store
        .seed(None, &JsString::from(session), &json(&persisted))
        .expect("#1473 the seeded persisted state must be accepted");
}

async fn seeded_store() -> Arc<PendingSopStore> {
    let store = Arc::new(PendingSopStore::new(root1_resolver()));
    let seeds: [(&str, i64, Option<&str>, &str, &str); 11] = [
        ("sess-handoff", 1, Some(HANDOFF_WAIT), HANDOFF_STATE, BUNDLE),
        (
            "sess-handoff2",
            1,
            Some(HANDOFF_WAIT),
            HANDOFF_STATE,
            BUNDLE,
        ),
        (
            "sess-handoff3",
            1,
            Some(HANDOFF_WAIT),
            HANDOFF_STATE,
            BUNDLE,
        ),
        (
            "sess-handoff4",
            1,
            Some(HANDOFF_WAIT),
            HANDOFF_STATE,
            BUNDLE,
        ),
        ("sess-done", 1, Some(HANDOFF_WAIT), HANDOFF_STATE, BUNDLE),
        ("sess-active", 3, None, ACTIVE_STATE, BUNDLE),
        ("sess-ext", 2, Some(EXT_WAIT), EXT_STATE, BUNDLE),
        ("sess-waitstale", 1, Some(STALE_WAIT), HANDOFF_STATE, BUNDLE),
        ("sess-rev", 7, Some(HANDOFF_WAIT), HANDOFF_STATE, BUNDLE),
        ("sess-badpin", 1, Some(BADPIN_WAIT), HANDOFF_STATE, BUNDLE),
        (
            "sess-unassigned",
            1,
            Some(HANDOFF_WAIT),
            HANDOFF_STATE,
            BUNDLE_UNASSIGNED,
        ),
    ];
    for (session, revision, wait, state, bundle) in seeds {
        seed_persisted(&store, session, revision, wait, state, bundle);
    }
    // A pre-migration legacy receipt: a stored resume result without the
    // `authorization` facts (it survives `isResumeResult`), so a human
    // replay of its requestId hits the legacy-receipt rejection. This seed
    // is a Rust-side store-level fixture (no TS generator counterpart —
    // the registered session catalog above stops at the recorder's set).
    store
        .seed(
            None,
            &JsString::from("sess-legacy"),
            &json(&format!(
                r#"{{"schemaVersion":3,"sessionId":"sess-legacy","bundle":{BUNDLE},"state":{HANDOFF_STATE},"revision":1,"wait":{HANDOFF_WAIT},"resumeRequests":{{"req-legacy":{{"accepted":true,"sessionId":"sess-legacy","requestId":"req-legacy","revision":2,"message":"legacy","waitId":"wait-1","source":"human"}}}},"updatedAt":"2026-01-01T00:00:00.000Z"}}"#
            )),
        )
        .expect("#1473 the legacy seeded persisted state must be accepted");
    let resumed = ApprovalPendingStore::resume(
        &*store,
        &StoreResumeInput {
            session_key: JsString::from("sess-done"),
            project_key: Some(JsValue::String(JsString::from("proj1"))),
            request_id: JsString::from("req-dup"),
            wait_id: JsString::from("wait-1"),
            message: JsString::from("done"),
            expected_revision: Some(1.0),
            slot_updates: None,
            source: ResumeSource::Human,
            authority: Some(StoreAuthority {
                tenant_id: JsString::from("t1"),
                session_id: JsString::from("sess-done"),
                subject: subject("staff-member").expect("#1350 the fixture staff member"),
            }),
        },
    )
    .await;
    assert!(
        matches!(&resumed, Ok(value) if matches!(value.get("duplicate"), Some(JsValue::Bool(false)))),
        "#1350 the sess-done pre-resume must succeed, got {resumed:?}"
    );
    store
}

// ----------------------------------------------------------------- harness

/// The approvals routing inputs of one fixture server, mirroring the
/// recorder's six: `main` (static binding over the seeded store), `dynamic`
/// (+ the scripted resolveBinding over its own store), `nomap` (no
/// resolveSession port), `bare` (no bridge at all), `stub` (the scripted
/// bridge object) and `nobind` (empty binding strings).
struct ServerSetup {
    approvals: Config,
    store: Option<Arc<PendingSopStore>>,
}

async fn server_setup(server: &str) -> ServerSetup {
    let token = TOKEN.to_owned();
    match server {
        "bare" => ServerSetup {
            approvals: Config {
                token,
                bridge: None,
            },
            store: None,
        },
        "stub" => ServerSetup {
            approvals: Config {
                token,
                bridge: Some(Arc::new(StubBridge(binding("t1", "a1", "u1")))),
            },
            store: None,
        },
        "dynamic" => {
            let store = seeded_store().await;
            ServerSetup {
                approvals: Config {
                    token,
                    bridge: Some(Arc::new(ApprovalBridgeImpl {
                        binding: binding("t1", "a1", "u1"),
                        resolve_binding: Some(Arc::new(DynamicResolver)),
                        authenticate: Arc::new(FixtureAuthority),
                        resolve_session: Some(Arc::new(Catalog)),
                        store: Arc::clone(&store) as Arc<dyn ApprovalPendingStore>,
                    })),
                },
                store: Some(store),
            }
        }
        _ => {
            let store = seeded_store().await;
            ServerSetup {
                approvals: Config {
                    token,
                    bridge: Some(Arc::new(ApprovalBridgeImpl {
                        binding: if server == "nobind" {
                            binding("", "", "")
                        } else {
                            binding("t1", "a1", "u1")
                        },
                        resolve_binding: None,
                        authenticate: Arc::new(FixtureAuthority),
                        resolve_session: if server == "nomap" {
                            None
                        } else {
                            Some(Arc::new(Catalog))
                        },
                        store: Arc::clone(&store) as Arc<dyn ApprovalPendingStore>,
                    })),
                },
                store: Some(store),
            }
        }
    }
}

/// Bind one native listener and write the raw request bytes to it, then
/// capture the raw response bytes like the TS recorder did (quiet window,
/// EOF, or socket error ends the capture). The approvals transport is
/// composed first, exactly where TS mounts `handlePublicApprovalHttpRequest`
/// in front of the module-host transport and every other route.
async fn serve_and_capture(
    approvals_config: Config,
    request: &[u8],
    id: &str,
    half_close: bool,
) -> (Vec<u8>, bool) {
    let module_config = crate::module_host::Config {
        token: TOKEN.to_owned(),
        capabilities: None,
    };
    let native_config = native_http::Config {
        token: TOKEN.to_owned(),
        loopback: true,
        static_root: None,
    };
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#1350 bind listener");
    let bound = listener.local_addr().expect("#1350 listener local address");
    let server_task = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("#1350 accept connection");
            let approvals_config = approvals_config.clone();
            let module_config = module_config.clone();
            let native_config = native_config.clone();
            tokio::spawn(async move {
                // The production path always carries a stop signal; a sender
                // that is dropped on the spot is the no-stop case.
                let (_, stop) = tokio::sync::watch::channel(false);
                let served = http_proxy::serve_connection_native_with_shutdown(
                    socket,
                    move |mut request, _lease| {
                        let approvals_config = approvals_config.clone();
                        let module_config = module_config.clone();
                        let native_config = native_config.clone();
                        async move {
                            match serve(&mut request, &approvals_config).await {
                                Some(answer) => answer,
                                None => {
                                    match crate::module_host::serve(&mut request, &module_config)
                                        .await
                                    {
                                        Some(answer) => answer,
                                        None => native_http::serve(&request, &native_config).await,
                                    }
                                }
                            }
                        }
                    },
                    stop,
                )
                .await;
                if let Err(error) = served {
                    // A handler Err (the over-budget and truncated-body cells)
                    // is the deliberate zero-byte close.
                    eprintln!("#1350 approvals connection error: {error}");
                }
            });
        }
    });
    let mut stream = timeout(DEADLINE, TcpStream::connect(bound))
        .await
        .expect("#1350 connect deadline")
        .expect("#1350 connect to approvals listener");
    stream
        .write_all(request)
        .await
        .expect("#1350 write request bytes");
    if half_close {
        stream.shutdown().await.expect("#1350 half-close");
    }
    let captured = capture(&mut stream, id).await;
    server_task.abort();
    captured
}

async fn capture(stream: &mut TcpStream, id: &str) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 8192];
    let start = std::time::Instant::now();
    let mut closed = false;
    while start.elapsed() < DEADLINE {
        let wait = if out.is_empty() {
            DEADLINE - start.elapsed()
        } else {
            QUIET
        };
        match timeout(wait, stream.read(&mut buffer)).await {
            Err(_elapsed) if !out.is_empty() => break,
            Err(_elapsed) => continue,
            Ok(Ok(0)) => {
                closed = true;
                break;
            }
            Ok(Ok(read)) => out.extend_from_slice(&buffer[..read]),
            // The TS recorder ignored socket errors and kept the bytes that
            // had arrived, so a reset while the response drains is a close.
            Ok(Err(_reset)) => {
                closed = true;
                break;
            }
        }
    }
    assert!(
        !out.is_empty() || closed,
        "#1350 cell {id} produced no bytes and stayed open"
    );
    (out, closed)
}

/// One cell through the fixture replay: capture the raw response and compare
/// the normalized bytes and the closed flag against the golden oracle. The
/// oracle stores its latin1-mapped text, so both sides are compared as the
/// wire bytes they map to 1:1.
async fn replay(id: &str) {
    let cell = cell(id);
    let setup = server_setup(&cell.server).await;
    let (raw, closed) =
        serve_and_capture(setup.approvals, &cell.request, &cell.id, cell.half_close).await;
    assert_eq!(
        normalize(&raw),
        normalize(&latin1_bytes(&cell.response)),
        "#1350 cell {} normalized response",
        cell.id
    );
    assert_eq!(
        closed, cell.closed,
        "#1350 cell {} connection closed flag",
        cell.id
    );
}

macro_rules! replay_tests {
    ($($function:ident => $id:literal),* $(,)?) => {
        /// Every cell id the macro instantiates; a test asserts this plus the
        /// pinned ids is exactly the oracle's cell set, so neither list can
        /// drift alone.
        const REPLAY_CELL_IDS: &[&str] = &[$($id),*];
        $(
            #[tokio::test]
            async fn $function() {
                replay($id).await;
            }
        )*
    };
}

replay_tests! {
    auth_missing => "auth-missing",
    auth_wrong => "auth-wrong",
    auth_lower => "auth-lower",
    auth_dblspace => "auth-dblspace",
    auth_dup => "auth-dup",
    auth_get_wrong => "auth-get-wrong",
    method_get => "method-get",
    method_delete => "method-delete",
    method_head => "method-head",
    route_trailing => "route-trailing",
    route_statuses => "route-statuses",
    route_pct => "route-pct",
    route_dot => "route-dot",
    route_upper => "route-upper",
    route_query => "route-query",
    body_bad_json => "body-bad-json",
    body_empty => "body-empty",
    body_number => "body-number",
    body_array => "body-array",
    body_null => "body-null",
    body_string => "body-string",
    whitelist_extra => "whitelist-extra",
    whitelist_approver => "whitelist-approver",
    scope_tenant => "scope-tenant",
    scope_agent => "scope-agent",
    scope_missing => "scope-missing",
    approver_missing => "approver-missing",
    approver_malformed => "approver-malformed",
    approver_empty_rest => "approver-empty-rest",
    approver_mixed_case => "approver-mixed-case",
    approver_wrong_bearer => "approver-wrong-bearer",
    subject_tenant => "subject-tenant",
    subject_disabled => "subject-disabled",
    subject_source => "subject-source",
    subject_role => "subject-role",
    subject_emptyuser => "subject-emptyuser",
    session_unknown => "session-unknown",
    session_project_mismatch => "session-project-mismatch",
    session_key_mismatch => "session-key-mismatch",
    nomap_status => "nomap-status",
    bare_status => "bare-status",
    bare_resume => "bare-resume",
    nobind_status => "nobind-status",
    status_handoff_admin => "status-handoff-admin",
    status_handoff_member => "status-handoff-member",
    status_handoff_other => "status-handoff-other",
    status_active => "status-active",
    status_missing => "status-missing",
    status_badpin => "status-badpin",
    status_unassigned => "status-unassigned",
    status_module_disabled => "status-module-disabled",
    status_after_resume => "status-after-resume",
    resume_success_member => "resume-success-member",
    resume_success_admin => "resume-success-admin",
    resume_duplicate => "resume-duplicate",
    resume_duplicate_message => "resume-duplicate-message",
    resume_duplicate_revision => "resume-duplicate-revision",
    resume_ext_source => "resume-ext-source",
    resume_wait_stale => "resume-wait-stale",
    resume_revision_conflict => "resume-revision-conflict",
    resume_not_found => "resume-not-found",
    resume_invalid_source => "resume-invalid-source",
    resume_invalid_message => "resume-invalid-message",
    resume_invalid_revision => "resume-invalid-revision",
    resume_invalid_negative => "resume-invalid-negative",
    resume_invalid_wait => "resume-invalid-wait",
    resume_member_forbidden => "resume-member-forbidden",
    dynamic_scope_skipped => "dynamic-scope-skipped",
    dynamic_other_binding => "dynamic-other-binding",
    dynamic_resolver_fail => "dynamic-resolver-fail",
    stub_snapshot => "stub-snapshot",
    stub_null => "stub-null",
    stub_undef => "stub-undef",
    stub_scope => "stub-scope",
    stub_echo_approver => "stub-echo-approver",
    stub_fail_status => "stub-fail-status",
    stub_fail_teapot => "stub-fail-teapot",
    stub_fail_unknown => "stub-fail-unknown",
    stub_fail_plain => "stub-fail-plain",
    stub_fail_codeonly => "stub-fail-codeonly",
    stub_resume_ok => "stub-resume-ok",
    stub_resume_undef => "stub-resume-undef",
    stub_resume_conflict => "stub-resume-conflict",
    stub_resume_413 => "stub-resume-413",
    budget_exact => "budget-exact",
    budget_over => "budget-over",
    conn_close => "conn-close",
    keep_alive_reuse => "keep-alive-reuse",
}

/// The cells pinned raw instead of replayed: their bytes carry a registered
/// transport or component diff no normalization may swallow.
const PINNED_CELL_IDS: &[&str] = &["http10", "premature-close", "resume-requestid-proto"];

/// #1350: the replay list plus the pinned list is the oracle's cell set; a
/// cell added to the oracle without a test (or vice versa) fails here.
#[test]
fn replay_cells_cover_exactly_the_oracle() {
    let recorded = oracle();
    let mut oracle_ids: Vec<&str> = recorded["cells"]
        .as_array()
        .expect("#1350 oracle cells array")
        .iter()
        .map(|cell| cell["id"].as_str().expect("#1350 oracle cell id"))
        .collect();
    oracle_ids.sort_unstable();
    let mut covered: Vec<&str> = REPLAY_CELL_IDS.to_vec();
    covered.extend_from_slice(PINNED_CELL_IDS);
    covered.sort_unstable();
    assert_eq!(
        covered, oracle_ids,
        "#1350 replay cell ids and oracle cell ids are the same set"
    );
}

/// #1350: an HTTP/1.0 request is answered with Node's own HTTP/1.1 status
/// line (oracle cell `http10`), while hyper writes its `HTTP/1.0` line — the
/// same registered diff #1209 pinned for this listener (ledger
/// A8.4-NATIVE-HTTP-001); everything else — the `Connection: close`, the
/// absent framing, the close-delimited body, the closed socket — replays.
#[tokio::test]
async fn http10_close_delimited_response() {
    let cell = cell("http10");
    let setup = server_setup(&cell.server).await;
    let (bytes, closed) =
        serve_and_capture(setup.approvals, &cell.request, &cell.id, cell.half_close).await;
    let raw = latin1_string(&bytes);
    assert!(
        raw.starts_with("HTTP/1.0 200 OK\r\n"),
        "#1350 pin http10: hyper answers with a 1.0 status line (Node's 1.1 line is the registered diff): {raw:?}"
    );
    assert!(
        raw.contains("\r\nConnection: close\r\n")
            && !raw.to_ascii_lowercase().contains("keep-alive")
            && !raw.to_ascii_lowercase().contains("transfer-encoding"),
        "#1350 pin http10: the 1.0 response is close-delimited with no keep-alive and no chunking like Node: {raw:?}"
    );
    let expected_body = cell
        .response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    assert!(
        raw.ends_with(expected_body),
        "#1350 pin http10: the 1.0 body is close-delimited, not chunked: {raw:?}"
    );
    assert!(
        closed,
        "#1350 pin http10: the 1.0 response closes the connection like Node"
    );
}

/// #1350: a request that closes before its declared body length is complete
/// draws Node's own parser-level `400 Connection: close` (oracle cell
/// `premature-close`), bytes the hyper typed response API cannot produce; the
/// transport itself writes nothing in either stack, and hyper's handler
/// error is the zero-byte close — the residual registered in
/// A8.4-MODULE-HOST-APPROVALS-001 (its 继承差异 section).
#[tokio::test]
async fn premature_close_is_a_zero_byte_close() {
    let cell = cell("premature-close");
    let setup = server_setup(&cell.server).await;
    let (raw, closed) =
        serve_and_capture(setup.approvals, &cell.request, &cell.id, cell.half_close).await;
    assert!(
        raw.is_empty(),
        "#1350 pin premature-close: the truncated body closes with zero bytes (Node answers its own parser-level 400 Connection: close — the registered A8.4-MODULE-HOST-APPROVALS-001 residual): {raw:?}"
    );
    assert!(
        closed,
        "#1350 pin premature-close: the truncated body closes the connection"
    );
}

/// #1350 (registered A8.4-MODULE-HOST-APPROVALS-001): TS
/// `current.resumeRequests[input.requestId]` walks `Object.prototype`, so a
/// `requestId` of `"constructor"` hits the `Object` constructor and the
/// duplicate branch reads a function without `authorization` — the golden
/// cell `resume-requestid-proto` records TS answering
/// 502 `SOP_APPROVAL_LEGACY_RECEIPT` (measured against baseline cc7c4e6).
/// The Rust store looks up own keys only, so the same request resumes
/// normally: the pinned 200 below is the registered divergence, not a
/// replay.
#[tokio::test]
async fn requestid_on_the_prototype_chain_diverges() {
    let golden = cell("resume-requestid-proto");
    assert!(
        golden.response.starts_with("HTTP/1.1 502")
            && golden
                .response
                .contains("{\"code\":\"SOP_APPROVAL_LEGACY_RECEIPT\"}"),
        "#1350 pin proto: the TS golden must record the legacy-receipt 502: {}",
        golden.response
    );
    let setup = server_setup(&golden.server).await;
    let (raw, closed) = serve_and_capture(
        setup.approvals,
        &golden.request,
        &golden.id,
        golden.half_close,
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "#1350 pin proto: the Rust store's own-key lookup must let the resume through (the TS prototype-chain 502 is the registered divergence): {response}"
    );
    assert!(
        response.contains("\"accepted\":true")
            && response.contains("\"duplicate\":false")
            && response.contains("\"requestId\":\"constructor\"")
            && response.contains("\"sessionId\":\"sess-handoff4\""),
        "#1350 pin proto: the Rust resume result must be the fresh acceptance of the 'constructor' request id: {response}"
    );
    assert!(
        !closed,
        "#1350 pin proto: the keep-alive response must leave the connection open"
    );
}

// -------------------------------------------------------------- normalize

/// The bytes both sides are compared in, the #1209 rules: the Date value is
/// masked, header names are case-folded (hyper renders Title-Case, TS's
/// `content-type` is literal lowercase — the registered transport diff), the
/// empty-body `content-length: 0` line is dropped, and chunk sizes are
/// case-folded. Status lines are compared verbatim — both stacks write
/// canonical `HTTP/1.1 200 OK` casing, and the one code without a canonical
/// reason (499) is answered with Node's `unknown` through hyper's
/// `ReasonPhrase` extension — and chunk bodies are never folded. Everything
/// works on the raw wire bytes: the approvals bodies are ASCII JSON, so the
/// latin1 text the oracle stores maps 1:1.
///
/// Sibling: keep in sync with `normalize` in `src/module_host/tests.rs`.
fn normalize(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    'responses: while !rest.is_empty() {
        let Some(head_end) = find(rest, b"\r\n\r\n") else {
            out.extend_from_slice(rest);
            break;
        };
        let head = &rest[..head_end];
        let mut chunked = false;
        let mut length = None;
        for line in split(head, b"\r\n").skip(1) {
            let Some((name, value)) = split_once(line, b":") else {
                continue;
            };
            let name = trim_ascii(name).to_ascii_lowercase();
            if name == b"transfer-encoding"
                && find(&value.to_ascii_lowercase(), b"chunked").is_some()
            {
                chunked = true;
            } else if name == b"content-length" {
                length = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.trim().parse::<usize>().ok());
            }
        }
        for (index, line) in split(head, b"\r\n").enumerate() {
            if index == 0 {
                out.extend_from_slice(line);
                out.extend_from_slice(b"\r\n");
                continue;
            }
            let Some((name, value)) = split_once(line, b":") else {
                out.extend_from_slice(line);
                out.extend_from_slice(b"\r\n");
                continue;
            };
            if name.eq_ignore_ascii_case(b"date") {
                out.extend_from_slice(b"date: <masked>\r\n");
            } else if name.eq_ignore_ascii_case(b"content-length") && trim_ascii(value) == b"0" {
                // hyper's implicit empty-body length.
            } else {
                out.extend_from_slice(&name.to_ascii_lowercase());
                out.extend_from_slice(b":");
                out.extend_from_slice(value);
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(b"\r\n");
        rest = &rest[head_end + 4..];
        if chunked {
            loop {
                let Some(line_end) = find(rest, b"\r\n") else {
                    out.extend_from_slice(rest);
                    break 'responses;
                };
                let field = split(&rest[..line_end], b";").next().unwrap_or(&[]);
                let size_text = String::from_utf8_lossy(field).into_owned();
                let size = usize::from_str_radix(size_text.trim(), 16)
                    .unwrap_or_else(|_| panic!("#1350 chunk size {size_text:?} is not hex"));
                out.extend_from_slice(size_text.to_ascii_lowercase().as_bytes());
                out.extend_from_slice(b"\r\n");
                rest = &rest[line_end + 2..];
                let chunk_end = size
                    .checked_add(2)
                    .expect("#1350 chunk size plus CRLF overflows usize");
                if rest.len() < chunk_end {
                    out.extend_from_slice(rest);
                    break 'responses;
                }
                out.extend_from_slice(&rest[..size]);
                out.extend_from_slice(b"\r\n");
                rest = &rest[chunk_end..];
                if size == 0 {
                    continue 'responses;
                }
            }
        }
        match length {
            Some(size) if rest.len() >= size => {
                out.extend_from_slice(&rest[..size]);
                rest = &rest[size..];
                continue 'responses;
            }
            _ => {
                out.extend_from_slice(rest);
                break;
            }
        }
    }
    out
}

/// Find the first occurrence of `needle` in `haystack`.
///
/// Sibling: keep in sync with the byte helpers in
/// `src/module_host/tests.rs`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Split on every `separator`, the `str::split` shape (a trailing separator
/// yields a final empty slice; an empty haystack yields nothing).
fn split<'a>(haystack: &'a [u8], separator: &[u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = haystack;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match find(rest, separator) {
            Some(at) => {
                let (line, tail) = rest.split_at(at);
                rest = &tail[separator.len()..];
                Some(line)
            }
            None => {
                let line = rest;
                rest = &[];
                Some(line)
            }
        }
    })
}

fn split_once<'a>(haystack: &'a [u8], separator: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    find(haystack, separator).map(|at| {
        let (before, after) = haystack.split_at(at);
        (before, &after[separator.len()..])
    })
}

/// Trim ASCII whitespace from both ends.
fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    &bytes[start..end]
}

/// The wire bytes the oracle's latin1-mapped response text names: every char
/// is one byte 0–255, so the map is 1:1.
fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars().map(|char| char as u8).collect()
}

fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

// ------------------------------------------------------- component tests

/// #1350: an empty configured bearer token must not admit a bare
/// `Authorization: Bearer ` (or any spacing): the TS comparison is
/// `!options.token || authorization !== \`Bearer ${token}\``, so an empty
/// token is always the 401 — and the 401 precedes both the method check and
/// the absent-bridge 503.
#[tokio::test]
async fn empty_token_admits_no_bare_bearer() {
    let approvals_config = Config {
        token: String::new(),
        bridge: None,
    };
    for authorization in [
        "Bearer ",
        "Bearer",
        "Bearer  ",
        " Bearer ",
        "Bearer fixture-token",
    ] {
        let request = format!(
            "POST /api/module-host/approvals/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {authorization}\r\nContent-Length: 2\r\n\r\n{{}}"
        );
        let (raw, _) = serve_and_capture(
            approvals_config.clone(),
            request.as_bytes(),
            "empty-token",
            false,
        )
        .await;
        let response = latin1_string(&raw);
        assert!(
            response.starts_with("HTTP/1.1 401")
                && response.contains("{\"code\":\"PUBLIC_HOST_TRANSPORT_UNAUTHORIZED\"}"),
            "#1350 an empty token must not admit `{authorization}`: {response}"
        );
    }
}

/// #1350: the resume shape gate runs before the bridge's `access()`, so a bad
/// resume body wins over the missing session mapping (the `nomap` server's
/// own 503) — the TS statement order of `publicApprovalBridge.resume`.
#[tokio::test]
async fn resume_validation_precedes_session_mapping() {
    let body = r#"{"tenantId":"t1","agentId":"a1","sessionKey":"sess-handoff","source":"external_task","requestId":"req-x","waitId":"wait-1","expectedRevision":1,"message":"m"}"#;
    let request = format!(
        "POST /api/module-host/approvals/resume HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let setup = server_setup("nomap").await;
    let (raw, _) = serve_and_capture(
        setup.approvals,
        request.as_bytes(),
        "resume-validation-order",
        false,
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 400")
            && response.contains("{\"code\":\"APPROVAL_INPUT_INVALID\"}"),
        "#1350 a bad resume shape must win over the missing session mapping: {response}"
    );
}

/// The store's status view, the same source the WS `sop_status` family reads.
async fn store_status(store: &Arc<PendingSopStore>, session: &str) -> JsValue {
    ApprovalPendingStore::status(
        store.as_ref(),
        &StoreStatusInput {
            session_key: JsString::from(session),
            project_key: Some(JsValue::String(JsString::from("proj1"))),
        },
    )
    .await
    .expect("#1350 the store status call resolves")
    .expect("#1350 the seeded session answers a status")
}

/// #1350: the A8.4 visibility requirement — one transport-driven resume
/// (the golden cell's own request bytes, over HTTP) is the state the WS side
/// reads: the same store answers `handoff`/revision 1 with the wait before
/// and `active`/revision 2 with the merged slots and no wait after.
#[tokio::test]
async fn transport_resume_is_visible_to_the_ws_side() {
    let setup = server_setup("main").await;
    let store = setup
        .store
        .clone()
        .expect("#1350 the main server runs over the pending store");
    let before = store_status(&store, "sess-handoff").await;
    assert_eq!(
        before.get("revision"),
        Some(&JsValue::Number(1.0)),
        "#1350 the seeded handoff session starts at revision 1"
    );
    assert!(
        matches!(before.get("wait"), Some(JsValue::Object(_))),
        "#1350 the seeded handoff session starts waiting"
    );
    assert_eq!(
        before.get("state").and_then(|state| state.get("status")),
        Some(&JsValue::String(JsString::from("handoff"))),
        "#1350 the seeded handoff session starts in the handoff state"
    );
    let cell = cell("resume-success-member");
    let (raw, _) =
        serve_and_capture(setup.approvals, &cell.request, &cell.id, cell.half_close).await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "#1350 the transport resume must succeed first: {response}"
    );
    let after = store_status(&store, "sess-handoff").await;
    assert_eq!(
        after.get("revision"),
        Some(&JsValue::Number(2.0)),
        "#1350 the WS-side store sees the resumed revision"
    );
    assert_eq!(
        after.get("wait"),
        None,
        "#1350 the WS-side store sees the cleared wait"
    );
    let state = after.get("state").expect("#1350 the resumed state");
    assert_eq!(
        state.get("status"),
        Some(&JsValue::String(JsString::from("active"))),
        "#1350 the WS-side store sees the active status"
    );
    assert_eq!(
        state.get("awaiting_input_json"),
        Some(&JsValue::Null),
        "#1350 the WS-side store sees the cleared awaiting input"
    );
    assert_eq!(
        stringify_json(
            &state
                .get("slots_json")
                .cloned()
                .unwrap_or(JsValue::Undefined)
        )
        .expect("#1350 the merged slots stringify")
        .unwrap_or_default(),
        r#"{"k":"v"}"#,
        "#1350 the WS-side store sees the merged slots"
    );
}

/// #1350: `isDeepStrictEqual` of the duplicate-receipt replay — own keys as
/// a set (never insertion order), `SameValue` primitives: `NaN` equals `NaN`,
/// `+0` does not equal `-0`, and a number never equals its string.
#[test]
fn deep_strict_equal_is_isdeepequal() {
    let left = json(r#"{"a":1,"b":[2,3],"c":{"d":"x"}}"#);
    let reordered = json(r#"{"c":{"d":"x"},"b":[2,3],"a":1}"#);
    assert!(
        deep_strict_equal(&left, &reordered),
        "#1350 isDeepStrictEqual compares own keys as a set, not in order"
    );
    let different = json(r#"{"a":1,"b":[2,3],"c":{"d":"y"}}"#);
    assert!(
        !deep_strict_equal(&left, &different),
        "#1350 isDeepStrictEqual rejects a different value"
    );
    let missing = json(r#"{"a":1,"b":[2,3]}"#);
    assert!(
        !deep_strict_equal(&left, &missing),
        "#1350 isDeepStrictEqual rejects a missing key"
    );
    let extra = json(r#"{"a":1,"b":[2,3],"c":{"d":"x"},"e":null}"#);
    assert!(
        !deep_strict_equal(&left, &extra),
        "#1350 isDeepStrictEqual rejects an extra key"
    );
    assert!(
        deep_strict_equal(&json("\"1\""), &json("\"1\"")),
        "#1350 isDeepStrictEqual accepts equal strings"
    );
    assert!(
        !deep_strict_equal(&json("1"), &json("\"1\"")),
        "#1350 isDeepStrictEqual never equates a number with its string"
    );
    assert!(
        !deep_strict_equal(&json("0"), &json("-0")),
        "#1350 isDeepStrictEqual treats +0 and -0 as different (SameValue)"
    );
    assert!(
        deep_strict_equal(&json("\"NaN\""), &json("\"NaN\"")),
        "#1350 the string NaN equals itself"
    );
    // SameValue: every NaN equals every NaN regardless of payload bits (JSON
    // cannot carry NaN, but a scripted bridge value can).
    assert!(
        deep_strict_equal(&JsValue::Number(f64::NAN), &JsValue::Number(f64::NAN)),
        "#1350 isDeepStrictEqual equates NaN with NaN"
    );
    assert!(
        deep_strict_equal(
            &JsValue::Number(f64::NAN),
            &JsValue::Number(f64::from_bits(0xFFF8_0000_0000_0001)),
        ),
        "#1350 isDeepStrictEqual equates NaN with any NaN bit pattern, not only identical bits"
    );
    assert!(
        !deep_strict_equal(&JsValue::Number(f64::NAN), &json("1")),
        "#1350 isDeepStrictEqual never equates NaN with a number"
    );
}

/// #996-style regeneration guard for the fixture; see the fixture README for
/// the pinned Node invocation.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_identically() {
    test_baseline::regenerates_identically("module-host-approvals");
}

// --------------------------------------------- fix-r1 component tests

/// #1350 fix-r1 (DS review P1): the catch branch's `{ code }` body must stay
/// valid JSON even when the `?? 'PUBLIC_APPROVAL_BRIDGE_FAILED'` default is
/// switched off and the code is `undefined` — `JSON.stringify(undefined)`
/// writes nothing, which produced the illegal `{"code":}` frame. `undefined`
/// now serializes as `null`; production TS always applies the default, so
/// the wire bytes of every real failure are unchanged.
#[test]
fn failure_body_is_valid_json_for_every_code() {
    assert_eq!(
        failure_body(&JsValue::Undefined),
        "{\"code\":null}",
        "#1350 an undefined code must serialize as null, never an empty insertion"
    );
    assert_eq!(
        failure_body(&JsValue::String(JsString::from("X_CODE"))),
        "{\"code\":\"X_CODE\"}"
    );
    assert_eq!(failure_body(&JsValue::Number(123.0)), "{\"code\":123}");
    for code in [
        JsValue::Undefined,
        JsValue::String(JsString::from("X_CODE")),
        JsValue::Number(123.0),
    ] {
        let body = failure_body(&code);
        parse_json(&body).expect("#1350 the failure body must parse as JSON");
    }
}

/// One direct store resume, the WS lane's own call shape (the shape
/// `native_process`'s `sop_resume` family will hold once #1313 wires it) —
/// the only caller that can reach the store branches the HTTP bridge's own
/// validation keeps out.
async fn store_resume(
    store: &Arc<PendingSopStore>,
    session: &str,
    request_id: &str,
    authority: Option<StoreAuthority>,
    expected_revision: Option<f64>,
) -> Result<JsValue, ApprovalFailure> {
    ApprovalPendingStore::resume(
        store.as_ref(),
        &StoreResumeInput {
            session_key: JsString::from(session),
            project_key: Some(JsValue::String(JsString::from("proj1"))),
            request_id: JsString::from(request_id),
            wait_id: JsString::from("wait-1"),
            message: JsString::from("approved"),
            expected_revision,
            slot_updates: None,
            source: ResumeSource::Human,
            authority,
        },
    )
    .await
}

fn member_authority(session: &str) -> StoreAuthority {
    StoreAuthority {
        tenant_id: JsString::from("t1"),
        session_id: JsString::from(session),
        subject: subject("staff-member").expect("#1350 the fixture staff member"),
    }
}

/// A store failure's code as a plain string, for assertions.
fn failure_code(failure: &ApprovalFailure) -> String {
    match &failure.code {
        Some(JsValue::String(code)) => code.to_utf8().unwrap_or_default(),
        _ => String::new(),
    }
}

/// One seeded session over a fresh store, the same persisted shape
/// [`seeded_store`] writes.
async fn single_session_store(
    session: &str,
    wait: &str,
    state: &str,
    bundle: &str,
) -> Arc<PendingSopStore> {
    let store = Arc::new(PendingSopStore::new(root1_resolver()));
    seed_persisted(&store, session, 1, Some(wait), state, bundle);
    store
}

/// #1350 fix-r1 (DS review P2): `sop.skill_id === wait.skillId || sop.id ===
/// wait.skillId` is one strict `===` per disjunct — a sop that merely *lacks*
/// a field never matches a *present* skillId, so this collision (one sop
/// matching on `skill_id` while another has an `id` of its own and no
/// `skill_id`) pins exactly one SOP, exactly like TS.
#[tokio::test]
async fn pinned_sop_match_needs_both_sides_of_the_strict_equal() {
    const WAIT: &str = r#"{"id":"wait-1","kind":"handoff","skillId":"skill-1","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}"#;
    const STATE: &str =
        r#"{"status":"handoff","active_skill_id":"skill-1","active_step_id":"step-1"}"#;
    const SOP: &str = r#""version":"1.0.0","content":{"nodes":[{"node_id":"step-1","assignee_user_id":"staff-member-id"}]}"#;
    let bundle =
        format!(r#"{{"sops":[{{"skill_id":"skill-1",{SOP}}},{{"id":"sop-other",{SOP}}}]}}"#);
    let store = single_session_store("sess-pin-collide", WAIT, STATE, &bundle).await;
    let snapshot = store_status(&store, "sess-pin-collide").await;
    assert!(
        matches!(snapshot.get("approval"), Some(JsValue::Object(_))),
        "#1350 only the skill_id match may count; the id-bearing skill_id-less sop must not collide: {}",
        stringify_json(&snapshot)
            .unwrap_or_default()
            .unwrap_or_default()
    );
    assert_eq!(
        snapshot.get("approval").and_then(|a| a.get("skillId")),
        Some(&JsValue::String(JsString::from("skill-1"))),
        "#1350 the pinned approval must be the skill_id-matching sop's"
    );
}

/// #1350 fix-r1 (DS review P2): with the wait's own `skillId` absent, a sop
/// matches through the disjunct naming the very field it lacks (`undefined
/// === undefined`), so one sop missing `skill_id` and another missing `id`
/// both match — the ambiguous pair TS and Rust alike reject as
/// `SOP_APPROVAL_PIN_INVALID`, while a single such sop still pins.
#[tokio::test]
async fn pinned_sop_collision_of_missing_fields_is_ambiguous() {
    const WAIT: &str = r#"{"id":"wait-1","kind":"handoff","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}"#;
    const STATE: &str = r#"{"status":"handoff","active_step_id":"step-1"}"#;
    const SOP: &str = r#""version":"1.0.0","content":{"nodes":[{"node_id":"step-1","assignee_user_id":"staff-member-id"}]}"#;
    let colliding =
        format!(r#"{{"sops":[{{"id":"sop-a",{SOP}}},{{"skill_id":"skill-x",{SOP}}}]}}"#);
    let store = single_session_store("sess-pin-ambiguous", WAIT, STATE, &colliding).await;
    let snapshot = store_status(&store, "sess-pin-ambiguous").await;
    let error = snapshot
        .get("approvalError")
        .expect("#1350 the ambiguous pin");
    assert_eq!(
        error.get("code"),
        Some(&JsValue::String(JsString::from("SOP_APPROVAL_PIN_INVALID"))),
        "#1350 two sops each matching through the missing-field disjunct must be ambiguous"
    );
    let single = format!(r#"{{"sops":[{{"id":"sop-a",{SOP}}}]}}"#);
    let store = single_session_store("sess-pin-single", WAIT, STATE, &single).await;
    let snapshot = store_status(&store, "sess-pin-single").await;
    assert!(
        matches!(snapshot.get("approval"), Some(JsValue::Object(_))),
        "#1350 a single sop missing skill_id still pins an absent skillId"
    );
}

/// #1350 fix-r1 (Opus review P2): the store branches no golden cell or test
/// reached — first `SOP_NOT_WAITING`, the late resume on an already-active
/// session (the wait is gone), over the real transport.
#[tokio::test]
async fn late_resume_on_an_active_session_is_not_waiting() {
    let body = r#"{"tenantId":"t1","agentId":"a1","sessionKey":"sess-active","source":"human","requestId":"req-late","waitId":"wait-1","expectedRevision":3,"message":"late"}"#;
    let request = format!(
        "POST /api/module-host/approvals/resume HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nx-staffdeck-approver-authorization: Bearer staff-admin\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let setup = server_setup("main").await;
    let (raw, _) = serve_and_capture(
        setup.approvals,
        request.as_bytes(),
        "resume-late-active",
        false,
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 502") && response.contains("{\"code\":\"SOP_NOT_WAITING\"}"),
        "#1350 a resume on an active session must be the SOP_NOT_WAITING 502: {response}"
    );
}

/// #1350 fix-r1 (Opus review P2): `SOP_APPROVAL_AUTH_REQUIRED` — the WS
/// lane's resume without an authority (the HTTP bridge always supplies one).
/// A `sopStateError` failure carries no status; the transport maps that
/// channel to 502 (the golden `resume-not-found` cell pins the mapping).
#[tokio::test]
async fn resume_without_authority_requires_auth() {
    let store = seeded_store().await;
    let failure = store_resume(&store, "sess-handoff", "req-auth", None, Some(1.0))
        .await
        .expect_err("#1350 an authority-less resume must fail");
    assert_eq!(
        failure.status, None,
        "#1350 a state error never carries its own status"
    );
    assert_eq!(
        failure_code(&failure),
        "SOP_APPROVAL_AUTH_REQUIRED",
        "#1350 the authority-less resume must demand the authority"
    );
}

/// #1350 fix-r1 (Opus review P2): `SOP_REVISION_REQUIRED` — a handoff resume
/// without `expectedRevision`. The HTTP bridge's own shape gate rejects that
/// input first (400 `APPROVAL_INPUT_INVALID`, the `resume-invalid-*` cells),
/// so this store branch belongs to the WS lane alone.
#[tokio::test]
async fn handoff_resume_without_expected_revision_requires_it() {
    let store = seeded_store().await;
    let failure = store_resume(
        &store,
        "sess-handoff2",
        "req-norev",
        Some(member_authority("sess-handoff2")),
        None,
    )
    .await
    .expect_err("#1350 a revision-less handoff resume must fail");
    assert_eq!(failure.status, None);
    assert_eq!(
        failure_code(&failure),
        "SOP_REVISION_REQUIRED",
        "#1350 the handoff resume must demand the original revision"
    );
}

/// #1350 fix-r1 (Opus review P2), fix-r2 (minimax sibling review P1, ruling
/// b): `SOP_APPROVAL_LEGACY_RECEIPT` — a stored receipt without
/// `authorization` facts (the pre-migration shape) replays as rejected for
/// the human source. A pure store-level assertion: the TS generator has no
/// `sess-legacy` seed (an unregistered session dies at `resolveSession`'s
/// 403 long before the store), so no golden cell carries these bytes and no
/// TS-side answer is claimed here — the cross-stack pairing waits for the
/// follow-up that seeds the session and records the cell.
#[tokio::test]
async fn legacy_receipt_replay_is_rejected() {
    let store = seeded_store().await;
    let failure = store_resume(
        &store,
        "sess-legacy",
        "req-legacy",
        Some(member_authority("sess-legacy")),
        Some(1.0),
    )
    .await
    .expect_err("#1350 the legacy receipt replay must be rejected");
    assert_eq!(
        failure.status, None,
        "#1350 a state error never carries its own status"
    );
    assert_eq!(
        failure_code(&failure),
        "SOP_APPROVAL_LEGACY_RECEIPT",
        "#1350 the legacy receipt replay must be rejected as the legacy shape"
    );
}

// ------------------------------------------------- #1473 store integrity

/// #1473: `seed` performs the TS `read` shape rejection (`isPersistedState`
/// plus the `sessionId` match) — an invalid persisted shape is a structured
/// `SOP_STATE_INVALID` failure, never a panic and never a silent
/// empty-object default.
#[test]
fn seed_rejects_invalid_persisted_shapes() {
    let store = PendingSopStore::new(root1_resolver());
    store
        .seed(
            None,
            &JsString::from("sess-shape"),
            &json(
                r#"{"schemaVersion":3,"sessionId":"sess-shape","bundle":{"sops":[]},"state":{}}"#,
            ),
        )
        .expect("#1473 the valid persisted shape must seed");
    let invalid = [
        // raw not a record
        "5",
        "\"text\"",
        "[1,2]",
        // state not a record / null / absent
        r#"{"schemaVersion":3,"sessionId":"sess-shape","bundle":{"sops":[]},"state":5}"#,
        r#"{"schemaVersion":3,"sessionId":"sess-shape","bundle":{"sops":[]},"state":null}"#,
        r#"{"schemaVersion":3,"sessionId":"sess-shape","bundle":{"sops":[]}}"#,
        // schemaVersion outside 1..3 / not a number
        r#"{"schemaVersion":4,"sessionId":"sess-shape","bundle":{"sops":[]},"state":{}}"#,
        r#"{"schemaVersion":"3","sessionId":"sess-shape","bundle":{"sops":[]},"state":{}}"#,
        r#"{"sessionId":"sess-shape","bundle":{"sops":[]},"state":{}}"#,
        // sessionId not a string / not the seeded one / absent
        r#"{"schemaVersion":3,"sessionId":5,"bundle":{"sops":[]},"state":{}}"#,
        r#"{"schemaVersion":3,"sessionId":"sess-other","bundle":{"sops":[]},"state":{}}"#,
        r#"{"schemaVersion":3,"bundle":{"sops":[]},"state":{}}"#,
        // bundle not a record / without a sops array / absent
        r#"{"schemaVersion":3,"sessionId":"sess-shape","bundle":5,"state":{}}"#,
        r#"{"schemaVersion":3,"sessionId":"sess-shape","bundle":{},"state":{}}"#,
        r#"{"schemaVersion":3,"sessionId":"sess-shape","state":{}}"#,
    ];
    for raw in invalid {
        let Err(failure) = store.seed(None, &JsString::from("sess-shape"), &json(raw)) else {
            panic!("#1473 the invalid persisted shape must be rejected: {raw}");
        };
        assert_eq!(
            failure.status, None,
            "#1473 the store-integrity failure carries no status of its own: {raw}"
        );
        assert_eq!(
            failure_code(&failure),
            "SOP_STATE_INVALID",
            "#1473 the invalid persisted shape must answer SOP_STATE_INVALID: {raw}"
        );
    }
}

/// #1473: the shape gate guards `seed`, but a corrupted in-memory state
/// (non-object, swapped in behind the gate) must still answer with the
/// structured `SOP_STATE_INVALID` failure — never a panic under the state
/// lock, the pre-fix whole-store outage. The
/// `approvals-resume-empty-default-on` mutant reinstates the historical
/// silent empty-object default and is killed by this test.
#[tokio::test]
async fn resume_answers_sop_state_invalid_for_a_corrupted_non_object_state() {
    let store = Arc::new(PendingSopStore::new(root1_resolver()));
    // An external-task wait: the resume path runs past the authority checks
    // and straight into the state mutation block.
    seed_persisted(&store, "sess-corrupt", 1, Some(EXT_WAIT), EXT_STATE, BUNDLE);
    super::pending::corrupt_seeded_state_for_test(&store, None, "sess-corrupt", json("5"));
    let Err(failure) = ApprovalPendingStore::resume(
        store.as_ref(),
        &StoreResumeInput {
            session_key: JsString::from("sess-corrupt"),
            project_key: Some(JsValue::String(JsString::from("proj1"))),
            request_id: JsString::from("req-corrupt"),
            wait_id: JsString::from("wait-ext"),
            message: JsString::from("approved"),
            expected_revision: Some(1.0),
            slot_updates: None,
            source: ResumeSource::ExternalTask,
            authority: None,
        },
    )
    .await
    else {
        panic!("#1473 the corrupted non-object state must be rejected, not resumed");
    };
    assert_eq!(
        failure_code(&failure),
        "SOP_STATE_INVALID",
        "#1473 the corrupted non-object state must answer SOP_STATE_INVALID, never panic"
    );
}

/// #1473: a rejected seed must leave the store usable — the shape rejection
/// runs before the state lock is taken and no resume path panics under it,
/// so the pre-fix Mutex-poisoning whole-store outage cannot recur.
#[tokio::test]
async fn resume_after_a_rejected_seed_still_works() {
    let store = Arc::new(PendingSopStore::new(root1_resolver()));
    let bad = json(r#"{"schemaVersion":3,"sessionId":"sess-bad","bundle":{"sops":[]},"state":5}"#);
    assert!(
        store.seed(None, &JsString::from("sess-bad"), &bad).is_err(),
        "#1473 the bad seed must be rejected"
    );
    seed_persisted(
        &store,
        "sess-bad",
        1,
        Some(HANDOFF_WAIT),
        HANDOFF_STATE,
        BUNDLE,
    );
    let result = store_resume(
        &store,
        "sess-bad",
        "req-after-bad",
        Some(member_authority("sess-bad")),
        Some(1.0),
    )
    .await
    .expect("#1473 the resume after a rejected seed must succeed");
    assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1473 the resume after a rejected seed is accepted"
    );
    let snapshot = store_status(&store, "sess-bad").await;
    assert_eq!(
        snapshot.get("revision"),
        Some(&JsValue::Number(2.0)),
        "#1473 the status after a rejected seed answers the resumed revision"
    );
    assert_eq!(
        snapshot.get("wait"),
        None,
        "#1473 the status after a rejected seed answers the cleared wait"
    );
}

/// #1473: a successful resume persists the incremented revision — the
/// `approvals-revision-off` mutant leaves the store at the old revision.
#[tokio::test]
async fn resume_success_increments_the_revision() {
    let store = Arc::new(PendingSopStore::new(root1_resolver()));
    seed_persisted(
        &store,
        "sess-rev1473",
        7,
        Some(HANDOFF_WAIT),
        HANDOFF_STATE,
        BUNDLE,
    );
    store_resume(
        &store,
        "sess-rev1473",
        "req-rev",
        Some(member_authority("sess-rev1473")),
        Some(7.0),
    )
    .await
    .expect("#1473 the revision-7 handoff resume must succeed");
    let snapshot = store_status(&store, "sess-rev1473").await;
    assert_eq!(
        snapshot.get("revision"),
        Some(&JsValue::Number(8.0)),
        "#1473 the resumed store must carry the incremented revision"
    );
}

/// #1473: the success mutation merges `slots_json` — the pre-resume slots
/// survive the resume's own updates, so the `approvals-slots-merge-off`
/// mutant that drops them is killed.
#[tokio::test]
async fn resume_success_merges_the_slots() {
    let store = Arc::new(PendingSopStore::new(root1_resolver()));
    const SLOTTED_STATE: &str = r#"{"status":"handoff","active_skill_id":"skill-1","active_step_id":"step-1","slots_json":{"existing":"1"}}"#;
    seed_persisted(
        &store,
        "sess-slots",
        1,
        Some(HANDOFF_WAIT),
        SLOTTED_STATE,
        BUNDLE,
    );
    let result = ApprovalPendingStore::resume(
        store.as_ref(),
        &StoreResumeInput {
            session_key: JsString::from("sess-slots"),
            project_key: Some(JsValue::String(JsString::from("proj1"))),
            request_id: JsString::from("req-slots"),
            wait_id: JsString::from("wait-1"),
            message: JsString::from("approved"),
            expected_revision: Some(1.0),
            slot_updates: Some(json(r#"{"k":"v"}"#)),
            source: ResumeSource::Human,
            authority: Some(member_authority("sess-slots")),
        },
    )
    .await
    .expect("#1473 the slotted handoff resume must succeed");
    assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1473 the slotted handoff resume is accepted"
    );
    let snapshot = store_status(&store, "sess-slots").await;
    let slots = snapshot
        .get("state")
        .and_then(|state| state.get("slots_json"))
        .expect("#1473 the resumed state must carry its slots_json");
    assert_eq!(
        slots.get("existing"),
        Some(&JsValue::String(JsString::from("1"))),
        "#1473 the pre-resume slots must survive the merge"
    );
    assert_eq!(
        slots.get("k"),
        Some(&JsValue::String(JsString::from("v"))),
        "#1473 the resume's own slot updates must land in the merge"
    );
}

/// #1473: the success mutation clears the wait — the
/// `approvals-wait-clear-off` mutant leaves the session waiting after a
/// successful resume.
#[tokio::test]
async fn resume_success_clears_the_wait() {
    let store = Arc::new(PendingSopStore::new(root1_resolver()));
    seed_persisted(
        &store,
        "sess-wait",
        1,
        Some(HANDOFF_WAIT),
        HANDOFF_STATE,
        BUNDLE,
    );
    store_resume(
        &store,
        "sess-wait",
        "req-wait",
        Some(member_authority("sess-wait")),
        Some(1.0),
    )
    .await
    .expect("#1473 the waiting handoff resume must succeed");
    let snapshot = store_status(&store, "sess-wait").await;
    assert_eq!(
        snapshot.get("wait"),
        None,
        "#1473 the resumed store must clear the wait"
    );
    assert_eq!(
        snapshot.get("state").and_then(|state| state.get("status")),
        Some(&JsValue::String(JsString::from("active"))),
        "#1473 the resumed state must be active"
    );
}
