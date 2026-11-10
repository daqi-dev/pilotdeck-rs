//! Explicit isolated process configuration and production gateway composition.
//! The A4 entry never reads these options unless its routing selects the
//! native composition: the first argument `--native` (#1071) or the
//! `PILOTDECK_GATEWAY_MODE` config item (#1354).
use crate::{ProxyError, config::Env, host::modules::tool_context::TurnEnvironment};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
mod agent;
mod agent_mcp;
mod assembly;
pub use assembly::start_with_manager_browser;
#[cfg(unix)]
mod background;
#[cfg(unix)]
mod background_archive;
#[cfg(test)]
mod channel_attachment_security_tests;
mod deployment;
mod local_host;
mod manager_browser;
#[cfg(test)]
mod manager_browser_ws_tests;
#[cfg(test)]
mod multi_project_history_tests;
mod project_scope;
mod project_storage;
mod projects;
mod public_host;
#[cfg(test)]
mod public_host_binding_tests;
#[cfg(test)]
mod public_host_catalog_tests;
#[cfg(test)]
mod public_host_disconnect_tests;
mod public_host_file;
#[cfg(test)]
mod public_host_skills_tests;
mod sdk_tools;
mod server_host;
mod sop_control;
#[cfg(test)]
mod sop_profile_tests;
pub use local_host::{LocalHost, LocalHostOptions};
pub use server_host::{
    ControlHandler, ControlReply, ControlRequest, HostExit, ServerOptions, legacy_local_bind_host,
    run_server,
};
// The advertised capability table lived as a literal inside `requests.rs`
// until #1352; it is its own module so the table, its exception list and the
// test that pins both to the recorded TS oracle sit next to each other.
mod capabilities;
mod channel_control;
mod channel_interaction;
// #1328 replay test (channel::weixin::replay_tests) drives the assembly
// directly, in the channel subtree rather than this one.
pub(crate) mod channels;
pub(crate) mod elicitation;
mod extensions;
mod families;
pub(crate) mod input_admission;
mod knowledge;
mod management;
mod methods;
mod notifications;
#[cfg(test)]
mod notifications_tests;
pub(crate) mod plan_todo;
mod skills;
pub use methods::registered_methods;
// #1320 fix-r2 F10: re-exported one hop up so the runtime-side single-source
// binding test (`runtime/settings/gateway_tests`) can see the family table.
#[cfg(test)]
pub(crate) use methods::METHODS as SETTINGS_FAMILY_METHODS;
#[cfg(test)]
mod channels_tests;
#[cfg(test)]
mod management_tests;
#[cfg(test)]
mod mcp_status_tests;
mod requests;
mod services;
mod session_events;
mod session_overrides;
// #1353 A8.5-pr2: the production switch faces over the real assembly.
#[cfg(test)]
mod settings_tests;
// #2363 item 6: crate-visible for the switch-side race test, which runs the
// cron routing seam's own dispatch body against the S1 stop face.
#[cfg(test)]
pub(crate) use methods::dispatch_under_management_gate;
mod switch_wiring;
#[cfg(test)]
mod switch_wiring_tests;
pub use session_overrides::SessionOverrides;
mod persisted_permissions;
mod sdk_ephemeral;
mod sdk_permission;
mod sdk_tool_policy;
mod tools;
mod web;
// #1433: the session MCP tool composition (SessionMcpRuntimeBundle) behind
// the turn registry — public so the integration test drives the production
// composition path (`tests/sdk_mcp_session_tools.rs`).
pub use tools::{
    SessionMcpRelease, SessionMcpTools, SessionMcpToolsOptions, enabled_session_mcp_servers,
    session_mcp_tools,
};

/// The fixed prefix of the switch machine's halt line: `run` prints
/// `"{NATIVE_SWITCH_HALTED}: {error}"` when the machine halts, and the
/// executable-level tests observe the child's stderr for the same prefix —
/// one home for the spelling, shared by production and tests (#2511).
pub const NATIVE_SWITCH_HALTED: &str = "native switch halted";

/// Start the real serial host, bind WS, then drain on either supported signal.
/// The listener never starts an upstream gateway/helper.
pub async fn run(options: Options) -> Result<(), ProxyError> {
    let token = std::fs::read_to_string(&options.token_file)?;
    let token = token.trim_end_matches(['\r', '\n']).to_owned();
    if crate::metadata::js_trim(&token).is_empty() {
        return Err("native token file must contain a nonempty token".into());
    }
    let inherited = capture_environment(std::env::vars_os());
    validate_environment(&inherited)?;
    let env = options.environment(inherited);
    server_host::run_embedding(
        options,
        env,
        server_host::Embedding {
            token,
            loopback: None,
            static_assets: None,
            control: None,
            restart: None,
            ready: Arc::new(|address| {
                Box::pin(async move {
                    eprintln!("native listening {address}");
                    Ok(())
                })
            }),
            native_diagnostics: true,
        },
    )
    .await
    .map(|_| ())
}

fn capture_environment(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Env {
    vars.into_iter()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// #2354: the `/api/web` route group's production inputs (TS
/// `handleWebApiRequest` over `options.gateway`): the same bearer the WS
/// lane checks, and the native metadata registry over the WS lane's home with
/// an empty cwd (the lazy process-cwd spelling `metadata_request` uses), so
/// the HTTP surface lists and describes the same bytes the WS RPCs do.
/// Extracted from `run` so the assembly pin in `tests.rs` composes the exact
/// production configuration instead of a test-local stand-in.
fn production_api_web(token: String, home: PathBuf) -> crate::api_web::Config {
    crate::api_web::Config {
        token,
        catalog: Arc::new(crate::api_web::NativeProjectCatalog::new(
            home,
            PathBuf::new(),
        )),
    }
}

/// #2354: the listener `run` serves — the same bind with the webhook prefix
/// check and the `/api/web` route group mounted on it exactly like
/// GatewayServer.ts (the webhook after `/auth/local-token`, api-web between
/// the webhook prefix check and the static fallback). Extracted from `run`
/// so the assembly has a test pin: deleting the `.with_api_web` mount answers
/// the static 404 in the e2e test instead of the project list, instead of
/// every listener test silently composing its own config.
async fn production_listener(
    listen: SocketAddr,
    host: crate::runtime::turn::gateway::TurnGateway,
    token: String,
    feishu_webhook: crate::native_http::WebhookResolver,
    api_web: crate::api_web::Config,
    loopback: Option<bool>,
) -> Result<crate::native_ws::NativeWsServer, ProxyError> {
    crate::native_ws::NativeWsServer::bind_with_policy(
        listen,
        host,
        token,
        env!("CARGO_PKG_VERSION").into(),
        loopback,
    )
    .await
    // #1329: `ANY /feishu/webhook` on the same listener (GatewayServer.ts
    // registers it on the HTTP server, after `/auth/local-token`).
    .map(|server| server.with_webhook(feishu_webhook).with_api_web(api_web))
}

/// All authorities are explicit; native mode has no default home, project,
/// token, upstream, or sidecar command. Port zero is supported for isolation.
/// `storage_root` maps to `PILOTDECK_STORAGE_ROOT` in the child environment;
/// absent means workspace snapshots are disabled for this process.
#[derive(Clone, Debug)]
pub struct Options {
    pub home: PathBuf,
    pub project: PathBuf,
    pub listen: SocketAddr,
    pub token_file: PathBuf,
    pub sidecar: PathBuf,
    pub sidecar_args: Vec<String>,
    pub drain_budget: Duration,
    pub storage_root: Option<String>,
}

pub fn native_requested(first: Option<&std::ffi::OsStr>) -> bool {
    first == Some(std::ffi::OsStr::new("--native"))
}

/// #1354 A8.5-pr3: the main entry's default routing switch, formalized from
/// #1071's explicit `--native` argument into a config item.
/// `PILOTDECK_GATEWAY_MODE` names the startup mode: `proxy` (the default,
/// #1303 option A — the Rust front with the TS upstream stays the rollback
/// target through stage A) or `native`. Unset keeps the default; every other
/// value — the empty string and non-UTF-8 bytes included — fails closed at
/// startup, the same shape as `PILOTDECK_NATIVE_RUN_REGISTRY` (#1369), and
/// the entry judges the item before the `--native` argument's routing, so the
/// rejection holds even when that argument is present. The explicit `--native`
/// argument wins over this config item, like the TS
/// baseline's own `readStringFlag(argv, "--host") ?? env.PILOTDECK_GATEWAY_HOST`
/// (src/cli/pilotdeck.ts:213): the per-invocation flag outranks the
/// deployment default.
pub fn gateway_mode(mode: Option<&std::ffi::OsStr>) -> Result<bool, ProxyError> {
    match mode {
        None => Ok(false),
        Some(value) => match value.to_str() {
            Some("proxy") => Ok(false),
            Some("native") => Ok(true),
            _ => {
                Err("gateway mode (PILOTDECK_GATEWAY_MODE) must be exactly proxy or native.".into())
            }
        },
    }
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Self, ProxyError> {
        Self::parse_at(args, Path::new(""))
    }

    fn parse_at(
        args: impl IntoIterator<Item = std::ffi::OsString>,
        cwd: &Path,
    ) -> Result<Self, ProxyError> {
        let mut args = args.into_iter();
        let mut values = std::collections::HashMap::new();
        let mut sidecar_args = Vec::new();
        while let Some(key) = args.next() {
            let key = key
                .into_string()
                .map_err(|_| "native option is not UTF-8")?;
            if ![
                "--home",
                "--project",
                "--listen",
                "--token-file",
                "--sidecar",
                "--sidecar-arg",
                "--drain-ms",
                "--storage-root",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("unknown native option: {key}").into());
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {key}"))?;
            if key == "--sidecar-arg" {
                sidecar_args.push(
                    value
                        .into_string()
                        .map_err(|_| "sidecar argument is not UTF-8")?,
                );
            } else if values.insert(key.clone(), value).is_some() {
                return Err(format!("duplicate native option: {key}").into());
            }
        }
        let take = |key: &str| {
            values
                .get(key)
                .ok_or_else(|| format!("native mode requires {key}"))
        };
        let home = directory(Path::new(take("--home")?), "home")?;
        // #2036: `--home` is judged on the canonical path `directory` returns,
        // not on the argv spelling alone. A UTF-8 argv can still resolve through
        // a symlink to a raw name; `environment()` then writes `HOME` /
        // `PILOT_HOME` with `to_string_lossy` while every other consumer uses
        // the raw `PathBuf`, silently handing the child a different home.
        utf8_argument(home.as_os_str(), "home")?;
        // Keep Node path.resolve identity (including symlink spelling). Only
        // the containment check uses the physical path, preserving isolation.
        let project = crate::metadata::resolve_process_path(cwd, Path::new(take("--project")?))?;
        // #1431 P2-3: Node's argv is a UTF-8 string, so the TS baseline can only
        // ever address a project by its lossy (`U+FFFD`) spelling. Accepting the
        // raw bytes here made `Services::workspace_id` hash that lossy spelling
        // while `serves_project` compares the raw `PathBuf`, so the resulting
        // workspace id was reachable by no `snapshot_list` request: the turn
        // captured snapshots nothing could list. Refuse the unreachable path at
        // parse, like `--storage-root` below. (`resolve_process_path` is
        // lexical, so the process cwd can contribute raw bytes as well.)
        utf8_argument(project.as_os_str(), "project")?;
        let physical_project = directory(&project, "project")?;
        if home.starts_with(&physical_project) || physical_project.starts_with(&home) {
            return Err("native home and project must be separate directories".into());
        }
        let listen: SocketAddr = take("--listen")?
            .to_str()
            .ok_or("listen is not UTF-8")?
            .parse()?;
        if listen.ip() != Ipv4Addr::LOCALHOST || (listen.port() != 0 && listen.port() < 1024) {
            return Err("native listen requires 127.0.0.1 and a high port (or 0)".into());
        }
        let token_file = file(Path::new(take("--token-file")?), "token file")?;
        let sidecar = file(Path::new(take("--sidecar")?), "sidecar")?;
        // #2036: `assembly` builds the child's command line from this path with
        // `to_string_lossy` (assembly.rs:398). Accepting raw bytes would launch
        // a different executable than the canonical path validated above, with
        // no error. Judge the canonical spelling, like `--home`.
        utf8_argument(sidecar.as_os_str(), "sidecar")?;
        let drain_ms = values
            .get("--drain-ms")
            .map(|s| {
                s.to_str()
                    .ok_or("drain budget is not UTF-8")?
                    .parse::<u64>()
                    .map_err(|_| "invalid drain budget")
            })
            .transpose()?
            .unwrap_or(5000);
        if drain_ms == 0 || drain_ms > 300_000 {
            return Err("native drain budget must be 1..300000 milliseconds".into());
        }
        Ok(Self {
            home,
            project,
            listen,
            token_file,
            sidecar,
            sidecar_args,
            drain_budget: Duration::from_millis(drain_ms),
            storage_root: values
                .get("--storage-root")
                .map(|v| utf8_argument(v, "storage root"))
                .transpose()?
                .map(str::to_owned),
        })
    }

    fn serves_project(&self, project: &str) -> bool {
        crate::metadata::resolve_process_path(Path::new(""), Path::new(project))
            .is_ok_and(|path| path == self.project)
    }

    /// A captured environment for config/children. The isolated home wins over
    /// inherited HOME/PILOT_HOME and no TS gateway upstream is ever consulted.
    /// When `--storage-root` was supplied it is injected as
    /// `PILOTDECK_STORAGE_ROOT` so the project's persistence stage builds a
    /// real snapshot provider; absent means snapshots remain disabled.
    /// `PILOTDECK_GATEWAY_MODE` is entry scope (#1354): this process already
    /// consumed it for its own routing, so it must not sink into the native
    /// subtree's environment — a descendant that re-executes the gateway
    /// would otherwise silently inherit a native routing it was never given.
    pub fn environment(&self, mut inherited: Env) -> Env {
        inherited.insert("HOME".into(), self.home.to_string_lossy().into_owned());
        inherited.insert(
            "PILOT_HOME".into(),
            self.home.to_string_lossy().into_owned(),
        );
        inherited.remove("PILOTDECK_PROXY_UPSTREAM");
        inherited.remove("PILOTDECK_GATEWAY_MODE");
        inherited.remove("PILOTDECK_CONFIG_PATH");
        if !inherited
            .get("PILOTDECK_AGENT_LOOP_TRANSPORT")
            .is_some_and(|value| crate::metadata::js_trim(value).eq_ignore_ascii_case("tcp"))
        {
            inherited.remove("PILOTDECK_AGENT_LOOP_TCP_HOST");
            inherited.remove("PILOTDECK_AGENT_LOOP_TCP_PORT");
        }
        match &self.storage_root {
            Some(root) => {
                inherited.insert("PILOTDECK_STORAGE_ROOT".into(), root.clone());
            }
            None => {
                inherited.remove("PILOTDECK_STORAGE_ROOT");
                inherited.remove("PILOTDECK_SNAPSHOT_ROOT");
            }
        }
        inherited
    }
}

fn absolute(path: &Path, label: &str) -> Result<PathBuf, ProxyError> {
    if !path.is_absolute() {
        return Err(format!("native {label} must be an absolute path").into());
    }
    Ok(path.canonicalize()?)
}
fn directory(path: &Path, label: &str) -> Result<PathBuf, ProxyError> {
    let path = absolute(path, label)?;
    if !path.is_dir() {
        return Err(format!("native {label} must be an existing directory").into());
    }
    Ok(path)
}
fn file(path: &Path, label: &str) -> Result<PathBuf, ProxyError> {
    let path = absolute(path, label)?;
    if !path.is_file() {
        return Err(format!("native {label} must be an existing file").into());
    }
    Ok(path)
}

/// #2036: one rule for every argv path field that reaches a lossy consumer.
/// Node's argv is a JS string, so the TS baseline can only ever name these
/// paths by their UTF-8 spelling; a raw byte spelling is a branch no client can
/// reach. Worse, the gateway keeps the raw `PathBuf` while children get
/// `to_string_lossy` (`HOME`, `PILOT_HOME`, the sidecar command line), so raw
/// bytes silently split one path into two directories. Refuse them at parse with
/// the field's name. Callers pass the value that actually lands in `Options` —
/// the canonical path for the filesystem fields, since resolution can surface
/// raw bytes even from a UTF-8 argv.
///
/// `--token-file` is deliberately exempt: `run()` reads it as a raw `PathBuf`
/// (`read_to_string`) and it never reaches a lossy consumer, so raw bytes there
/// cannot split one path into two.
fn utf8_argument<'a>(value: &'a std::ffi::OsStr, label: &str) -> Result<&'a str, ProxyError> {
    value
        .to_str()
        .ok_or_else(|| format!("{label} is not UTF-8").into())
}

fn validate_environment(env: &Env) -> Result<(), ProxyError> {
    for key in [
        "PILOTDECK_MANAGED_CONFIG",
        "PILOTDECK_STORAGE_ROOT",
        "PILOTDECK_SNAPSHOT_ROOT",
    ] {
        if env.get(key).is_some_and(|v| !v.is_empty()) {
            return Err(format!("native isolated mode does not accept inherited {key}").into());
        }
    }
    Ok(())
}

/// Bind the established host environment policy to the production tool port.
/// Capturing the base avoids reading a mutable process environment per tool.
pub struct NativeTurnEnvironment(pub JsObject);
impl TurnEnvironment for NativeTurnEnvironment {
    fn build(
        &self,
        base: Option<&JsValue>,
        cwd: &JsString,
        session: &JsString,
        turn: &JsString,
    ) -> Result<JsObject, crate::host::HostError> {
        let base = match base {
            None | Some(JsValue::Undefined | JsValue::Null) => &self.0,
            Some(JsValue::Object(base)) => base,
            Some(_) => {
                return Err(crate::host::ModuleError::new(
                    "Turn base environment must be an object.",
                )
                .into());
            }
        };
        crate::host::modules::lifecycle::build_turn_environment(Some(base), cwd, session, turn)
    }
}

#[cfg(test)]
mod services_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod channel_lifecycle_tests;
#[cfg(all(test, unix))]
mod hard_endpoint_interaction_tests;
#[cfg(all(test, unix))]
#[path = "channel/telegram/actual_host_tests.rs"]
mod telegram_actual_host_tests;

mod channel_search;

#[cfg(test)]
mod channel_command_fixture;
#[cfg(unix)]
mod channel_system;

#[cfg(test)]
#[path = "channel/feishu/actual_host_tests.rs"]
mod feishu_actual_host_tests;
#[cfg(test)]
#[path = "channel/slack/actual_host_tests.rs"]
mod slack_actual_host_tests;
#[cfg(test)]
#[path = "channel/sms/actual_host_tests.rs"]
mod sms_actual_host_tests;
#[cfg(test)]
#[path = "channel/wecom/actual_elicitation_tests.rs"]
mod wecom_actual_elicitation_tests;
#[cfg(test)]
#[path = "channel/wecom/actual_host_tests.rs"]
mod wecom_actual_host_tests;
#[cfg(test)]
#[path = "channel/wecom/actual_media_tests.rs"]
mod wecom_actual_media_tests;
#[cfg(test)]
#[path = "channel/wecom/actual_permission_allow_tests.rs"]
mod wecom_actual_permission_allow_tests;
#[cfg(test)]
#[path = "channel/wecom/actual_permission_tests.rs"]
mod wecom_actual_permission_tests;
#[cfg(test)]
#[path = "channel/wecom_callback/actual_host_tests.rs"]
mod wecom_callback_actual_host_tests;
#[cfg(test)]
#[path = "channel/weixin/actual_host_tests.rs"]
mod weixin_actual_host_tests;
#[cfg(test)]
#[path = "channel/weixin/media_host_tests.rs"]
mod weixin_media_host_tests;

#[cfg(unix)]
mod session_history;

/// Shared native source/installation asset discovery. The runtime plugin loader
/// uses the same candidate order as startup without importing private assembly.
pub(crate) fn builtin_skills_root_at(
    project: &std::path::Path,
    env: &crate::config::Env,
    executable_dir: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    assembly::builtin_skills_root_at(project, env, executable_dir)
}
