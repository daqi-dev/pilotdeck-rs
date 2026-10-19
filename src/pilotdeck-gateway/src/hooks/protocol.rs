//! Hook wire contracts (fixed TS `extension/hooks/protocol/*`).
//!
//! Open records use `JsObject` so unknown fields, insertion order, undefined,
//! non-finite numbers and UTF-16 strings survive the same boundary as TS.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub const HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "UserPromptSubmit",
    "PreModelRequest",
    "SessionStart",
    "SessionEnd",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "PermissionRequest",
    "PermissionDenied",
    "Setup",
    "ConfigChange",
    "InstructionsLoaded",
    "CwdChanged",
    "FileChanged",
    "WorktreeCreate",
    "WorktreeRemove",
    "Elicitation",
    "ElicitationResult",
];
pub const NOT_APPLICABLE_LEGACY_HOOK_EVENTS: &[&str] =
    &["TeammateIdle", "TaskCreated", "TaskCompleted"];

pub fn is_hook_event(value: &JsString) -> bool {
    HOOK_EVENTS.iter().any(|event| value.equals_str(event))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HookCommon {
    pub condition: Option<JsString>,
    pub status_message: Option<JsString>,
    pub once: Option<bool>,
    pub timeout: Option<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookShell {
    Bash,
    Powershell,
}
#[derive(Clone, Debug, PartialEq)]
pub enum HookCommand {
    Command {
        command: JsString,
        shell: Option<HookShell>,
        asynchronous: Option<bool>,
        async_rewake: Option<bool>,
        common: HookCommon,
    },
    Prompt {
        prompt: JsString,
        model: Option<JsString>,
        common: HookCommon,
    },
    Http {
        url: JsString,
        headers: Option<JsObject>,
        allowed_env_vars: Option<Vec<JsString>>,
        common: HookCommon,
    },
    Agent {
        prompt: JsString,
        model: Option<JsString>,
        common: HookCommon,
    },
    /// Runtime registration only; the persistent parser diagnoses and drops it.
    Callback {
        name: JsString,
        condition: Option<JsString>,
        status_message: Option<JsString>,
    },
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HookMatcher {
    pub matcher: Option<JsString>,
    pub plugin_name: Option<JsString>,
    pub plugin_id: Option<JsString>,
    pub plugin_root: Option<JsString>,
    pub hooks: Vec<HookCommand>,
}
/// Entries retain event insertion order; only supported events appear here.
pub type HooksSettings = Vec<(JsString, Vec<HookMatcher>)>;

impl HookCommand {
    pub fn to_value(&self) -> JsValue {
        let mut out = JsObject::new();
        let common = match self {
            Self::Command {
                command,
                shell,
                asynchronous,
                async_rewake,
                common,
            } => {
                out.insert("type".into(), "command".into());
                out.insert("command".into(), JsValue::String(command.clone()));
                if let Some(shell) = shell {
                    out.insert(
                        "shell".into(),
                        match shell {
                            HookShell::Bash => "bash",
                            HookShell::Powershell => "powershell",
                        }
                        .into(),
                    );
                }
                put_bool(&mut out, "async", *asynchronous);
                put_bool(&mut out, "asyncRewake", *async_rewake);
                common
            }
            Self::Prompt {
                prompt,
                model,
                common,
            }
            | Self::Agent {
                prompt,
                model,
                common,
            } => {
                out.insert(
                    "type".into(),
                    if matches!(self, Self::Prompt { .. }) {
                        "prompt"
                    } else {
                        "agent"
                    }
                    .into(),
                );
                out.insert("prompt".into(), JsValue::String(prompt.clone()));
                put_string(&mut out, "model", model);
                common
            }
            Self::Http {
                url,
                headers,
                allowed_env_vars,
                common,
            } => {
                out.insert("type".into(), "http".into());
                out.insert("url".into(), JsValue::String(url.clone()));
                if let Some(headers) = headers {
                    out.insert("headers".into(), JsValue::Object(headers.clone()));
                }
                if let Some(vars) = allowed_env_vars {
                    out.insert(
                        "allowedEnvVars".into(),
                        JsValue::Array(vars.iter().cloned().map(JsValue::String).collect()),
                    );
                }
                common
            }
            Self::Callback {
                name,
                condition,
                status_message,
            } => {
                out.insert("type".into(), "callback".into());
                out.insert("name".into(), JsValue::String(name.clone()));
                put_string(&mut out, "if", condition);
                put_string(&mut out, "statusMessage", status_message);
                return JsValue::Object(out);
            }
        };
        put_string(&mut out, "if", &common.condition);
        put_string(&mut out, "statusMessage", &common.status_message);
        put_bool(&mut out, "once", common.once);
        if let Some(timeout) = common.timeout {
            out.insert("timeout".into(), JsValue::Number(timeout));
        }
        JsValue::Object(out)
    }
}
impl HookMatcher {
    pub fn to_value(&self) -> JsValue {
        let mut out = JsObject::new();
        put_string(&mut out, "matcher", &self.matcher);
        put_string(&mut out, "pluginName", &self.plugin_name);
        put_string(&mut out, "pluginId", &self.plugin_id);
        put_string(&mut out, "pluginRoot", &self.plugin_root);
        out.insert(
            "hooks".into(),
            JsValue::Array(self.hooks.iter().map(HookCommand::to_value).collect()),
        );
        JsValue::Object(out)
    }
}
fn put_string(out: &mut JsObject, key: &str, value: &Option<JsString>) {
    if let Some(value) = value {
        out.insert(key.into(), JsValue::String(value.clone()));
    }
}
fn put_bool(out: &mut JsObject, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        out.insert(key.into(), JsValue::Bool(value));
    }
}

pub type HookInput = JsObject;
pub type HookBaseInput = JsObject;

/// Payload may override base fields; the explicit event always wins last.
pub fn create_hook_input(event: &JsString, base: &HookBaseInput, payload: &JsObject) -> HookInput {
    let mut out = JsObject::new();
    for (key, value) in base.entries().iter().chain(payload.entries()) {
        out.insert(key.clone(), value.clone());
    }
    out.insert("hookEventName".into(), JsValue::String(event.clone()));
    out
}

/// Only top-level ASCII capitals are converted. Nested payloads remain intact.
pub fn to_legacy_hook_input(input: &HookInput) -> JsObject {
    let reserved = [
        "hookEventName",
        "sessionId",
        "transcriptPath",
        "permissionMode",
        "agentId",
        "agentType",
    ];
    let mut out = JsObject::new();
    for (key, value) in ordered_entries(input) {
        if reserved.iter().any(|name| key.equals_str(name)) {
            continue;
        }
        let mut snake = Vec::new();
        for &c in key.units() {
            if (65..=90).contains(&c) {
                snake.push(95);
                snake.push(c + 32);
            } else {
                snake.push(c);
            }
        }
        let key = JsString::from_utf16(snake);
        // Assignment to {}.__proto__ invokes its setter, not an own property.
        if !key.equals_str("__proto__") {
            out.insert(key, value.clone());
        }
    }
    for (old, new) in [
        ("hookEventName", "hook_event_name"),
        ("sessionId", "session_id"),
        ("transcriptPath", "transcript_path"),
        ("cwd", "cwd"),
    ] {
        out.insert(
            new.into(),
            input
                .get_property(old)
                .unwrap_or(&JsValue::Undefined)
                .clone(),
        );
    }
    for (old, new) in [
        ("permissionMode", "permission_mode"),
        ("agentId", "agent_id"),
        ("agentType", "agent_type"),
    ] {
        if let Some(value) = input.get_property(old).filter(|v| !v.is_undefined()) {
            out.insert(new.into(), value.clone());
        }
    }
    out
}

/// Object.entries enumerates canonical array-index keys before other keys.
pub(super) fn ordered_entries(object: &JsObject) -> Vec<(&JsString, &JsValue)> {
    let mut entries: Vec<_> = object.entries().iter().map(|(k, v)| (k, v)).collect();
    entries.sort_by_key(|(key, _)| key.array_index().map_or((1, 0), |n| (0, n)));
    entries
}

#[derive(Clone, Debug, PartialEq)]
pub enum PermissionHookDecision {
    Allow {
        updated_input: Option<JsObject>,
        updated_permissions: Option<Vec<JsValue>>,
    },
    Deny {
        message: Option<JsString>,
        interrupt: Option<bool>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
    Passthrough,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookDecision {
    Approve,
    Block,
}
#[derive(Clone, Debug, PartialEq)]
pub struct HookSpecificOutput {
    pub hook_event_name: JsString,
    pub additional_context: Option<JsString>,
    pub initial_user_message: Option<JsString>,
    pub watch_paths: Option<Vec<JsString>>,
    pub permission_decision: Option<PermissionDecision>,
    pub permission_decision_reason: Option<JsString>,
    pub updated_input: Option<JsObject>,
    pub updated_mcp_tool_output: Option<JsValue>,
    pub decision: Option<PermissionHookDecision>,
    pub retry: Option<bool>,
    pub worktree_path: Option<JsString>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HookSyncOutput {
    pub continue_: Option<bool>,
    pub suppress_output: Option<bool>,
    pub stop_reason: Option<JsString>,
    pub decision: Option<HookDecision>,
    pub reason: Option<JsString>,
    pub system_message: Option<JsString>,
    pub specific: Option<HookSpecificOutput>,
    pub raw: Option<JsValue>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HookAsyncOutput {
    pub invocation_id: Option<JsString>,
    pub timeout_ms: Option<f64>,
    pub raw: Option<JsValue>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum HookOutput {
    Sync(Box<HookSyncOutput>),
    Async(HookAsyncOutput),
}
