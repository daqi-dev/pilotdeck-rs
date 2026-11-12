//! Explicit isolated process configuration and production gateway composition.
//! The A4 entry never reads these options unless its routing selects the
//! native composition: the first argument `--native` (#1071) or the
//! `PILOTDECK_GATEWAY_MODE` config item (#1354).
use crate::{ProxyError, host::modules::tool_context::TurnEnvironment};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
#[cfg(test)]
use crate::config::Env;
#[cfg(test)]
use std::{net::Ipv4Addr, path::Path, time::Duration};

mod options;
use options::{capture_environment, validate_environment};
pub use options::{Options, gateway_mode, native_requested};
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
mod router_events;
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
