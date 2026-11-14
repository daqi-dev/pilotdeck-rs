//! #1432: `mcp_server_status` routing tests — set → status through the real
//! native request router (`GatewayRequests::request`, the same admission
//! gate and family lookup production uses). The state machine itself
//! (`SdkMcpConfig`) is replayed against the TS oracle by
//! `tests/sdk_mcp.rs`; what this file pins is that the router reads the
//! same shared config the four #1319 control methods write, instead of the
//! #1311 isolated-mode constant empty list. Every TS expectation below was
//! first produced by the Node v22.22.0 probe on the fixed baseline
//! (`_tmp/20261108-archive-wt-1432/20261107-issue-1432/ts-probe.out.txt`,
//! commit cc7c4e653b) and
//! cross-checked against `tests/fixtures/sdk-mcp/oracle.json` (scenarios
//! basic / errors / resolve-error / status-order).
use super::Options;
use super::requests::Requests;
use crate::config::Env;
use crate::config::permission::GatewaySessionPermissionRuleSetRegistry;
use crate::runtime::connection::RequestError;
use crate::runtime::mcp::gateway::{McpGateway, McpGatewayOptions};
use crate::runtime::mcp::sdk::{
    SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpEnvironment, SdkMcpError,
    SdkMcpFuture, SdkMcpProject, SdkMcpProjectResolver, SdkMcpSessionPlugins,
};
use crate::runtime::memory::gateway::MemoryGateway;
use crate::runtime::plugins::LoadedPlugin;
use crate::runtime::project::{
    ProjectRuntimeFactory, ProjectRuntimeRegistry, ResourceBundle, RuntimeResult,
};
use crate::runtime::replacement::ReplacementCoordinator;
use crate::runtime::sdk_session::SdkSessionProjects;
use crate::runtime::session::{SessionOwner, SessionOwnerOptions};
use crate::runtime::turn::gateway::GatewayRequests;
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn text(value: &JsValue) -> String {
    stringify_json(value)
        .expect("#1432 routing value must be serializable")
        .expect("#1432 routing value must not be undefined")
}

// ────────────────────────────────────────────────────── the recording probe

#[derive(Default)]
struct ProbeState {
    events: Mutex<Vec<String>>,
    refresh_error: Mutex<Option<String>>,
    resolve_error: Mutex<Option<String>>,
}
#[derive(Clone, Default)]
struct Probe(Arc<ProbeState>);
impl Probe {
    fn event(&self, event: impl Into<String>) {
        lock(&self.0.events).push(event.into());
    }
    fn events(&self) -> Vec<String> {
        lock(&self.0.events).clone()
    }
    fn clear_events(&self) {
        lock(&self.0.events).clear();
    }
    fn fail_refresh_with(&self, message: &str) {
        *lock(&self.0.refresh_error) = Some(message.to_string());
    }
    fn fail_resolve_with(&self, message: &str) {
        *lock(&self.0.resolve_error) = Some(message.to_string());
    }
}

/// `runtime.pluginRuntime`: refresh / snapshot record, refresh can fail.
struct ProbeProject {
    probe: Probe,
}
impl SdkMcpProject for ProbeProject {
    fn project_root(&self) -> JsString {
        "/pdrs-fixture/project".into()
    }
    fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
        let probe = self.probe.clone();
        Box::pin(async move {
            probe.event("refresh");
            match lock(&probe.0.refresh_error).clone() {
                Some(message) => Err(SdkMcpError::plain("Error", message)),
                None => Ok(()),
            }
        })
    }
    fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<LoadedPlugin>>> {
        let probe = self.probe.clone();
        Box::pin(async move {
            probe.event("snapshot");
            Vec::new()
        })
    }
}

/// `this.resolve(input.projectKey)`: records, resolves any key, can fail.
struct ProbeResolver {
    probe: Probe,
}
impl SdkMcpProjectResolver for ProbeResolver {
    fn resolve<'a>(
        &'a self,
        project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        let probe = self.probe.clone();
        let recorded = project_key
            .map(|key| {
                key.to_utf8()
                    .map(|text| text.to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| "null".to_string());
        Box::pin(async move {
            probe.event(format!("resolve:{recorded}"));
            match lock(&probe.0.resolve_error).clone() {
                Some(message) => Err(SdkMcpError::plain("Error", message)),
                None => Ok(Arc::new(ProbeProject {
                    probe: probe.clone(),
                }) as Arc<dyn SdkMcpProject>),
            }
        })
    }
}

/// The native process has no SDK session plugin sources; same contract as
/// the production `NoSessionPlugins` (assembly/mcp.rs).
struct NoSessionPlugins;
impl SdkMcpSessionPlugins for NoSessionPlugins {
    fn plugins(&self, _session_key: &JsValue) -> Vec<Arc<LoadedPlugin>> {
        Vec::new()
    }
}

/// Pinned environment: no `${env:...}` expansion in these configs.
struct FixedEnvironment;
impl SdkMcpEnvironment for FixedEnvironment {
    fn env(&self, _name: &JsString) -> Option<JsString> {
        None
    }
    fn home(&self) -> JsString {
        "/pdrs-fixture/home".into()
    }
}

// ─────────────────────────────────────────────── the Requests under test

/// A factory that must never run: nothing in this replay stages a project
/// runtime or creates a session (the settings family uses the same shape).
struct NoRuntimeFactory;
impl ProjectRuntimeFactory for NoRuntimeFactory {
    fn stage(&self, _: &Path, _: &mut ResourceBundle) -> RuntimeResult {
        panic!("#1432 the mcp status replay never stages a runtime")
    }
}
impl crate::runtime::session::SessionFactory for NoRuntimeFactory {
    fn create(
        &self,
        _: crate::runtime::session::SessionContext,
        _: Option<crate::runtime::session::LiveSessionHandle>,
    ) -> crate::runtime::refresh::BoxFuture<
        Result<crate::runtime::session::LiveSessionHandle, crate::runtime::project::RuntimeError>,
    > {
        panic!("#1432 the mcp status replay never creates a session")
    }
}

struct Scratch(PathBuf);
impl Scratch {
    // #2335: the guard must exist before the first fallible disk op — a
    // panic inside the build loop otherwise leaks the temp tree, because
    // only a constructed `Scratch` unwinds through `Drop`. The canonical
    // path is written back into the guard; a second `Self(...)` would
    // drop-and-delete the tree it still points at.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-1432-{}", uuid::Uuid::new_v4()));
        let mut scratch = Self(root);
        for name in ["home", "project"] {
            std::fs::create_dir_all(scratch.0.join(name))
                .expect("#1432 scratch directories must be created");
        }
        for name in ["token", "sidecar"] {
            std::fs::write(scratch.0.join(name), "fixture")
                .expect("#1432 scratch file must be written");
        }
        let canonical = scratch
            .0
            .canonicalize()
            .expect("#1432 scratch canonical path");
        scratch.0 = canonical;
        scratch
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    requests: Requests,
    probe: Probe,
    owner: SessionOwner,
    // Owning the scratch keeps the temp tree cleaned on every path: an
    // early `expect` failure during construction unwinds through
    // `Scratch::drop` too (DS r1 concurrency: the previous early
    // `mem::forget` leaked the tree whenever the harness failed to build).
    _scratch: Scratch,
}

fn harness() -> Harness {
    let scratch = Scratch::new();
    let root = scratch.0.clone();
    let options = Options::parse([
        "--home".into(),
        root.join("home").into_os_string(),
        "--project".into(),
        root.join("project").into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        root.join("token").into_os_string(),
        "--sidecar".into(),
        root.join("sidecar").into_os_string(),
    ])
    .expect("#1432 the native options must parse");
    let mut owner_options = SessionOwnerOptions::new(Arc::new(NoRuntimeFactory));
    owner_options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(owner_options);
    let probe = Probe::default();

    // The #1319 close port: router close first, then the record — the same
    // order as `tests/fixtures/mcp-gateway`'s harness.
    let close_owner = owner.clone();
    let close_probe = probe.clone();
    let close_session: SdkMcpCloseSession = Arc::new(move |session_key, reason| {
        let key = match &session_key {
            JsValue::String(key) => key.clone(),
            _ => JsString::from(""),
        };
        let owner = close_owner.clone();
        let probe = close_probe.clone();
        let reason = reason.to_string();
        Box::pin(async move {
            owner
                .close(&key)
                .await
                .map_err(|error| SdkMcpError::plain("Error", JsString::from(error.message)))?;
            probe.event(format!(
                "close-session:{}:{reason}",
                key.to_utf8()
                    .unwrap_or_else(|_| panic!("#1432 session key must be valid UTF-8"))
            ));
            Ok(())
        })
    });

    let config = Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(ProbeResolver {
            probe: probe.clone(),
        }),
        close_session,
        session_plugins: Arc::new(NoSessionPlugins),
        permission_rule_sets: Arc::new(GatewaySessionPermissionRuleSetRegistry::new()),
        environment: Arc::new(FixedEnvironment),
        projects: SdkSessionProjects::shared(),
    }));
    let mcp = Arc::new(McpGateway::new(McpGatewayOptions {
        owner: owner.clone(),
        config,
    }));

    let env = options.environment(Env::new());
    let project = Arc::new(ProjectRuntimeRegistry::new(
        options.project.clone(),
        Arc::new(NoRuntimeFactory),
    ));
    // #1444: the real submit-lane services so a steer carrying attachments
    // composes through prepare_attachment_turn.
    let submit_services = Requests::stub_submit_services(&options, &project, &env, &owner);
    let requests = Requests {
        env,
        options: options.clone(),
        snapshot_enabled: false,
        memory: Arc::new(MemoryGateway::disabled()),
        coordinator: Arc::default(),
        runs: None,
        owner: owner.clone(),
        project,
        replacement: Arc::new(ReplacementCoordinator::new(
            Default::default(),
            Arc::new(owner.clone()),
            5000,
            None,
        )),
        recorders: Arc::default(),
        interactions: Arc::default(),
        mcp,
        async_hook_registry: None,
        // #1600: production assembles a connected bus; this stub must match.
        user_dialog_bus: Arc::new(Mutex::new(
            crate::user_dialog::GatewayUserDialogBus::connected(
                crate::user_dialog::DialogRuntime::default(),
            ),
        )),
        upload_gateway: Arc::new(crate::upload::gateway::UploadGateway::unavailable()),
        prepare_weixin_login: None,
        cron: None,
        always_on: None,
        settings: Requests::stub_settings(&owner),
        session_overrides: Arc::default(),
        submit_services,
    };
    Harness {
        requests,
        probe,
        owner,
        _scratch: scratch,
    }
}

/// One routed call: the full `GatewayRequests::request` path (isolation
/// admission + family lookup), not a direct method call.
async fn call(requests: &Requests, method: &str, params: &str) -> Result<JsValue, RequestError> {
    requests
        .request(
            method.into(),
            parse_json(params).expect("#1432 routing params must parse"),
        )
        .await
}

const SET_A: &str = r#"{
    "sessionKey": "s",
    "servers": {"a": {"type": "stdio", "command": "node", "args": ["server.js"]}}
}"#;

/// The issue's scenario: after a successful `set_mcp_servers`, the same
/// session's status read reports the stored server as `configured`, not the
/// #1311 constant empty list.
#[tokio::test]
async fn set_then_status_reports_the_session_servers() {
    let harness = harness();
    let set = call(&harness.requests, "set_mcp_servers", SET_A)
        .await
        .expect("#1432 set_mcp_servers must succeed");
    assert_eq!(
        text(&set),
        r#"{"added":["a"],"removed":[],"errors":[]}"#,
        "#1432 set_mcp_servers must add server a"
    );
    let status = call(
        &harness.requests,
        "mcp_server_status",
        &format!(
            r#"{{"projectKey":"{}","sessionKey":"s"}}"#,
            harness.requests.options.project.to_string_lossy()
        ),
    )
    .await
    .expect("#1432 mcp_server_status must succeed for the owning session");
    assert_eq!(
        text(&status),
        r#"{"servers":[{"name":"a","status":"configured"}]}"#,
        "#1432 the same session must read its stored server back as configured"
    );
    let events = harness.probe.events();
    for expected in [
        "resolve:".to_string() + &harness.requests.options.project.to_string_lossy(),
        "refresh".to_string(),
        "snapshot".to_string(),
        "close-session:s:sdk_mcp_servers_changed".to_string(),
        "resolve:".to_string() + &harness.requests.options.project.to_string_lossy(),
        "refresh".to_string(),
    ] {
        assert!(
            events.contains(&expected),
            "#1432 the routed set→status must drive the shared config (missing {expected:?} in {events:?})"
        );
    }
    harness
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}

/// A different session's status read stays empty: the projection is scoped
/// per session key (`status-order` in the TS oracle).
#[tokio::test]
async fn status_is_scoped_to_the_owning_session() {
    let harness = harness();
    call(&harness.requests, "set_mcp_servers", SET_A)
        .await
        .expect("#1432 set_mcp_servers must succeed before the scoping read");
    let other = call(
        &harness.requests,
        "mcp_server_status",
        r#"{"sessionKey":"t"}"#,
    )
    .await
    .expect("#1432 mcp_server_status must succeed for another session");
    assert_eq!(
        text(&other),
        r#"{"servers":[]}"#,
        "#1432 another session must not see session s servers"
    );
    harness
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}

/// `mcp_server_toggle` writes the same shared config, so the disabled
/// state is what the status read reports (`basic` in the TS oracle).
#[tokio::test]
async fn a_toggled_server_reports_disabled() {
    let harness = harness();
    call(&harness.requests, "set_mcp_servers", SET_A)
        .await
        .expect("#1432 set_mcp_servers must succeed before the toggle");
    let toggled = call(
        &harness.requests,
        "mcp_server_toggle",
        r#"{"sessionKey":"s","serverName":"a","enabled":false}"#,
    )
    .await
    .expect("#1432 mcp_server_toggle must succeed");
    assert_eq!(
        text(&toggled),
        r#"{"ok":true}"#,
        "#1432 mcp_server_toggle must answer the WS ok wrapper"
    );
    let status = call(
        &harness.requests,
        "mcp_server_status",
        r#"{"sessionKey":"s"}"#,
    )
    .await
    .expect("#1432 mcp_server_status must succeed after the toggle");
    assert_eq!(
        text(&status),
        r#"{"servers":[{"name":"a","status":"disabled"}]}"#,
        "#1432 a disabled server must read back as disabled"
    );
    harness
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}

/// No (or empty/null) sessionKey: empty projection, but `resolve` and
/// `refresh` still run before the session gate — the TS probe shows the
/// `resolve, refresh` pair even for the keyless read.
#[tokio::test]
async fn status_without_a_session_key_still_resolves_and_refreshes() {
    let harness = harness();
    call(&harness.requests, "set_mcp_servers", SET_A)
        .await
        .expect("#1432 set_mcp_servers must succeed before the keyless read");
    for params in ["{}", r#"{"sessionKey":""}"#, r#"{"sessionKey":null}"#] {
        harness.probe.clear_events();
        let status = call(&harness.requests, "mcp_server_status", params)
            .await
            .expect("#1432 mcp_server_status must succeed without a session key");
        assert_eq!(
            text(&status),
            r#"{"servers":[]}"#,
            "#1432 a falsy sessionKey must project an empty list ({params})"
        );
        assert_eq!(
            harness.probe.events(),
            vec!["resolve:null".to_string(), "refresh".to_string()],
            "#1432 the keyless status read must still resolve and refresh ({params})"
        );
    }
    harness
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}

/// The TS `errors` scenario: a refresh failure surfaces from the status
/// read (mapped through the family's `gateway_request_failed` fallback).
#[tokio::test]
async fn a_refresh_failure_surfaces_through_the_router() {
    let harness = harness();
    call(&harness.requests, "set_mcp_servers", SET_A)
        .await
        .expect("#1432 set_mcp_servers must succeed before the failing read");
    harness.probe.fail_refresh_with("refresh boom");
    let error = call(
        &harness.requests,
        "mcp_server_status",
        r#"{"sessionKey":"s"}"#,
    )
    .await
    .expect_err("#1432 a refresh failure must fail the status read");
    assert_eq!(
        "gateway_request_failed", error.code,
        "#1432 the plain Error keeps the family fallback code"
    );
    assert_eq!(
        error
            .message
            .to_utf8()
            .expect("#1432 refresh failure message must be UTF-8"),
        "refresh boom",
        "#1432 the refresh failure message must surface verbatim"
    );
    assert_eq!(
        harness.probe.events()[harness.probe.events().len() - 2..].to_vec(),
        vec!["resolve:null".to_string(), "refresh".to_string()],
        "#1432 the failing read must reach resolve and refresh"
    );
    harness
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}

/// The TS `resolve-error` scenario: a resolve failure fails the status
/// read before any refresh runs.
#[tokio::test]
async fn a_resolve_failure_surfaces_through_the_router() {
    let harness = harness();
    harness.probe.fail_resolve_with("resolve boom");
    let error = call(
        &harness.requests,
        "mcp_server_status",
        r#"{"sessionKey":"s"}"#,
    )
    .await
    .expect_err("#1432 a resolve failure must fail the status read");
    assert_eq!(
        "gateway_request_failed", error.code,
        "#1432 the resolve failure keeps the family fallback code"
    );
    assert_eq!(
        error
            .message
            .to_utf8()
            .expect("#1432 resolve failure message must be UTF-8"),
        "resolve boom",
        "#1432 the resolve failure message must surface verbatim"
    );
    assert_eq!(
        harness.probe.events(),
        vec!["resolve:null".to_string()],
        "#1432 the resolve failure must stop before refresh"
    );
    harness
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}

/// `input.projectKey` read on a nullish `input`: the TypeError the TS probe
/// shows (`Cannot read properties of undefined (reading 'projectKey')`).
#[tokio::test]
async fn nullish_params_report_the_type_error() {
    let harness = harness();
    let error = call(&harness.requests, "mcp_server_status", "null")
        .await
        .expect_err("#1432 nullish params must fail the status read");
    assert_eq!(
        "gateway_request_failed", error.code,
        "#1432 the TypeError has no gateway code, so the family fallback applies"
    );
    assert_eq!(
        error
            .message
            .to_utf8()
            .expect("#1432 type error message must be UTF-8"),
        "Cannot read properties of null (reading 'projectKey')",
        "#1432 the nullish input must report the property read TypeError"
    );
    assert!(
        harness.probe.events().is_empty(),
        "#1432 a nullish input must not reach the resolver"
    );
    harness
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}

/// The two admission shapes of `projectKey` on this route, after #2272 made
/// the gate treat an explicit `null` as "not provided": a `null` is admitted
/// and takes the shared config's nullish fallback — the same contract the
/// config itself implements (`runtime/mcp/sdk.rs`: `project_key` returns
/// `Ok(None)` for null, so the resolver sees no key and refreshes the
/// configured project, replaying TS `resolve(projectKey ?? fallback)` in
/// `ProjectRuntimeRegistry.ts`; probe
/// `_tmp/20261109-archive-wt-2272/20261108-issue-2272-fix1/ts-probe/probe-null.out` shows the same
/// `{"servers":[]}` answer for null and a missing key). A non-string key is
/// still an unserved key for the gate and is refused before the shared
/// config's own `ERR_INVALID_ARG_TYPE` (node:path.resolve) — that one
/// divergence is the #1311-era isolation admission this route shares with
/// every other method, not something the #1432 wiring introduces. These two
/// cells pin the current admission answer so the divergence stays visible
/// instead of drifting.
#[tokio::test]
async fn null_project_key_falls_back_and_non_string_stays_refused_by_admission() {
    // null: admitted, and the read falls back like a missing key (#2272).
    let admitted = harness();
    let status = call(
        &admitted.requests,
        "mcp_server_status",
        r#"{"projectKey":null,"sessionKey":"s"}"#,
    )
    .await
    .expect("#2272 a null projectKey must be admitted and answered");
    assert_eq!(
        text(&status),
        r#"{"servers":[]}"#,
        "#2272 the null projectKey read answers the empty session projection"
    );
    assert_eq!(
        admitted.probe.events(),
        vec!["resolve:null".to_string(), "refresh".to_string()],
        "#2272 a null projectKey must reach the resolver keyless and refresh"
    );
    admitted
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");

    // non-string: still an unserved key for the isolation gate.
    let refused = harness();
    let error = call(
        &refused.requests,
        "mcp_server_status",
        r#"{"projectKey":5,"sessionKey":"s"}"#,
    )
    .await
    .expect_err("#1432 admission must refuse non-string project keys");
    assert_eq!(
        "gateway_request_failed", error.code,
        "#1432 the admission refusal carries the family fallback code"
    );
    assert_eq!(
        error
            .message
            .to_utf8()
            .expect("#1432 admission message must be UTF-8"),
        "The isolated native process serves only its configured project.",
        "#1432 the isolation gate answers before the shared config"
    );
    assert!(
        refused.probe.events().is_empty(),
        "#1432 a refused key must not reach the resolver"
    );
    refused
        .owner
        .shutdown()
        .await
        .expect("#1432 harness owner shutdown");
}
