//! `lifecycle.dispatch` host adapter (TS `sidecarDefaultModuleDispatcher.ts:378-391,472`).
//!
//! The host rebuilds the ambient turn environment; no process environment or
//! hook runtime object crosses the sidecar boundary. Serialization is the
//! dispatcher's exact field order, including `pendingAsyncHooks`.
use super::wire;
use crate::hooks::protocol::is_hook_event;
use crate::host::{HostFuture, JsObject, JsString, JsValue, ModuleError};
use crate::runtime::hooks::{
    Cancellation, LifecycleDispatchInput, LifecycleRuntime, exec::process_env,
    runtime::serialize_lifecycle_dispatch_result,
};
use std::path::{Component, Path, PathBuf};

/// Ambient turn fields the dispatcher supplies to `lifecycle.dispatch`.
#[derive(Clone)]
pub struct Binding {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub cwd: JsString,
    pub permission_mode: Option<JsString>,
    /// Host config environment, or the process environment when absent.
    pub base_env: Option<JsObject>,
    /// Turn abort signal; never taken from sidecar payload authority.
    pub signal: Option<Cancellation>,
}

#[derive(Clone)]
struct Ports {
    runtime: LifecycleRuntime,
    binding: Binding,
}

/// One handler belongs to one turn/execute. The default value has no runtime
/// and fails closed instead of silently treating dispatch as empty.
#[derive(Clone, Default)]
pub struct Handler {
    ports: Option<Ports>,
}
impl Handler {
    pub fn new(runtime: LifecycleRuntime, binding: Binding) -> Self {
        Self {
            ports: Some(Ports { runtime, binding }),
        }
    }
    /// TS `NullLifecycleRuntime`: dispatch is empty. This handler's `dispose` is
    /// a no-op (the dispatcher never disposes the session runtime).
    pub fn null(binding: Binding) -> Self {
        Self::new(LifecycleRuntime::null(), binding)
    }
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let handler = self.clone();
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            if !operation.equals_str("dispatch") {
                return Err(ModuleError::new("Unsupported sidecar lifecycle operation.").into());
            }
            let event = wire::string_field(payload, "event")?;
            if !is_hook_event(event) {
                return Err(wire::named_error(
                    "Unsupported sidecar lifecycle event: ",
                    event,
                    "",
                ));
            }
            let payload_value = payload.and_then(|payload| payload.get("payload"));
            let payload = match payload_value {
                None | Some(JsValue::Undefined) => None,
                Some(JsValue::Object(object)) => Some(object.clone()),
                Some(_) => {
                    return Err(ModuleError::new("Lifecycle payload must be an object.").into());
                }
            };
            let ports = handler
                .ports
                .ok_or_else(|| ModuleError::new("Host did not provide a lifecycle runtime."))?;
            let base_input = base_input(&ports.binding);
            let env = build_turn_environment(
                ports.binding.base_env.as_ref(),
                &ports.binding.cwd,
                &ports.binding.session_id,
                &ports.binding.turn_id,
            );
            let result = ports
                .runtime
                .dispatch(LifecycleDispatchInput {
                    event: event.clone(),
                    base_input,
                    payload,
                    match_query: Some(event.clone()),
                    cwd: ports.binding.cwd.clone(),
                    env: Some(env),
                    signal: ports.binding.signal.clone(),
                })
                .await?;
            let mut response = JsObject::new();
            response.insert(
                "result".into(),
                serialize_lifecycle_dispatch_result(&result),
            );
            Ok(response)
        })
    }
    /// TS `sidecarDefaultModuleDispatcher.dispose()` only clears turn-local
    /// model streams/preparations; it never disposes the session-owned
    /// `LifecycleRuntime` (that belongs to `AgentRuntimeScope`, only when it
    /// owns the lifecycle). This handler is reconstructed each turn, so
    /// disposing the runtime here would drop pending async hooks and callback
    /// registrations between turns.
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

fn base_input(binding: &Binding) -> JsObject {
    let mut input = JsObject::new();
    input.insert(
        "sessionId".into(),
        JsValue::String(binding.session_id.clone()),
    );
    input.insert("transcriptPath".into(), "".into());
    input.insert("cwd".into(), JsValue::String(binding.cwd.clone()));
    if let Some(mode) = &binding.permission_mode {
        input.insert("permissionMode".into(), JsValue::String(mode.clone()));
    }
    input
}

/// TS `buildTurnEnvironment`: base env (or process env) plus the three
/// turn-scoped variables. `PILOTDECK_WORK_DIR` is resolved against the cwd.
pub fn build_turn_environment(
    base_env: Option<&JsObject>,
    cwd: &JsString,
    session_id: &JsString,
    turn_id: &JsString,
) -> JsObject {
    let mut env = base_env.cloned().unwrap_or_else(process_env);
    env.insert(
        "PILOTDECK_SESSION_ID".into(),
        JsValue::String(session_id.clone()),
    );
    env.insert("PILOTDECK_TURN_ID".into(), JsValue::String(turn_id.clone()));
    let work_dir = resolve_path(cwd)
        .join(".pilotdeck")
        .join("work")
        .join(path_segment(session_id))
        .join(path_segment(turn_id));
    env.insert(
        "PILOTDECK_WORK_DIR".into(),
        JsValue::String(work_dir.to_string_lossy().into_owned().into()),
    );
    env
}

fn resolve_path(cwd: &JsString) -> PathBuf {
    let cwd = String::from_utf16_lossy(cwd.units());
    let path = Path::new(&cwd);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    normalize(&absolute)
}

fn normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => output.push(prefix.as_os_str()),
            Component::RootDir => output.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // Node's normalize clamps `..` at an absolute root.
                if output.parent().is_none() {
                    continue;
                }
                output.pop();
            }
            Component::Normal(value) => output.push(value),
        }
    }
    output
}

/// TS `safeWorkPathSegment`. NFKC is approximated for the compatibility folds
/// that become ASCII (fullwidth forms and U+3000); other compatibility
/// characters remain a documented gap because adding a normalizer crate is
/// outside this change's dependency budget. Every other non-ASCII run becomes
/// `-`, so the result stays inside `[a-zA-Z0-9._-]`.
fn path_segment(value: &JsString) -> String {
    let mut folded = Vec::with_capacity(value.units().len());
    for &unit in value.units() {
        folded.push(match unit {
            0xFF01..=0xFF5E => unit - 0xFEE0,
            0x3000 => u16::from(b' '),
            other => other,
        });
    }
    let mut output = String::new();
    let mut dash = false;
    for &unit in &folded {
        let allowed = matches!(unit, 48..=57 | 65..=90 | 97..=122 | 46 | 95 | 45);
        if allowed {
            if dash {
                output.push('-');
                dash = false;
            }
            output.push(char::from(unit as u8));
        } else {
            dash = true;
        }
    }
    if dash && !output.is_empty() {
        output.push('-');
    }
    let output = output
        .trim_matches(|c: char| c == '-' || c == '.')
        .to_owned();
    let mut truncated: String = output.chars().take(96).collect();
    if truncated.is_empty() {
        truncated = "unknown".into();
    }
    truncated
}

/// TS `HostLifecycleRuntime.parseDispatchResult` boundary check. The module
/// responder itself always returns the canonical shape; this is used by the
/// replay oracle and future sidecar-client code.
pub fn validate_dispatch_result(value: &JsValue) -> Result<(), JsString> {
    crate::runtime::hooks::runtime::validate_lifecycle_dispatch_result(value)
}

/// TS `moduleFailure` projection: message precedence and the error code.
pub fn module_failure(value: &JsValue) -> (JsString, Option<JsString>) {
    crate::runtime::hooks::runtime::lifecycle_module_failure(value)
}
