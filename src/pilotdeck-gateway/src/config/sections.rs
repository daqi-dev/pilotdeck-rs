use super::{Diagnostics, Error, model::Provider, value::*};
use pilotdeck_protocol::{JsString, JsValue};
fn shape(raw: &JsValue, d: &mut Diagnostics, code: &str, msg: &str, path: &str) -> bool {
    if raw.is_undefined() {
        false
    } else if !record(raw) {
        d.fatal(code, msg, path);
        false
    } else {
        true
    }
}
fn unknown(
    raw: &JsValue,
    allowed: &[&str],
    d: &mut Diagnostics,
    code: &str,
    prefix: &str,
    description: &str,
) {
    for (k, _) in entries(raw) {
        let k = js_string(&JsValue::String(k.clone()));
        if !allowed.contains(&k.as_str()) {
            d.warn(
                code,
                format!("Unknown {description} {k}."),
                &format!("{prefix}.{k}"),
            );
        }
    }
}
fn selection(
    value: &JsValue,
    path: &str,
    providers: &[Provider],
    d: &mut Diagnostics,
) -> Result<(JsString, JsString), Error> {
    let Some(s) = value.as_string().filter(|s| !s.is_empty()) else {
        d.fatal(
            "CONFIG_AGENT_MODEL_INVALID",
            format!("{path} must be a non-empty provider/model string."),
            path,
        );
        d.check()?;
        unreachable!()
    };
    let pair = s
        .units()
        .iter()
        .position(|u| *u == 47)
        .filter(|i| *i > 0 && *i + 1 < s.units().len());
    let Some(i) = pair else {
        d.fatal(
            "CONFIG_AGENT_MODEL_INVALID",
            format!("{path} must use provider/model format."),
            path,
        );
        d.check()?;
        unreachable!()
    };
    let p = JsString::from_utf16(s.units()[..i].to_vec());
    let m = JsString::from_utf16(s.units()[i + 1..].to_vec());
    if let Some(provider) = providers.iter().find(|provider| provider.id == p) {
        if !provider.models.iter().any(|model| model.id == m) {
            d.fatal(
                "CONFIG_AGENT_MODEL_NOT_FOUND",
                format!(
                    "{path} references unknown model {} for provider {}.",
                    js_string(&JsValue::String(m.clone())),
                    js_string(&JsValue::String(p.clone()))
                ),
                path,
            );
        }
    } else {
        if builtin_property(&p) {
            let mut e = Error::new(
                "gateway_request_failed",
                format!(
                    "Cannot read properties of undefined (reading '{}')",
                    js_string(&JsValue::String(m.clone()))
                ),
            );
            e.name = "TypeError";
            return Err(e);
        }
        d.fatal(
            "CONFIG_AGENT_PROVIDER_NOT_FOUND",
            format!(
                "{path} references unknown provider {}.",
                js_string(&JsValue::String(p.clone()))
            ),
            path,
        );
    }
    d.check()?;
    Ok((p, m))
}
fn immediate_number(
    raw: &JsValue,
    path: &str,
    code: &str,
    positive: bool,
    label: &str,
) -> Result<(), Error> {
    if !raw.is_undefined() && !number(raw, positive, false) {
        return Err(Error::new(code, format!("{path} must be a {label}.")));
    }
    Ok(())
}
pub(super) fn agent(
    raw: &JsValue,
    providers: &[Provider],
    d: &mut Diagnostics,
) -> Result<(JsString, JsString), Error> {
    if !record(raw) {
        d.fatal(
            "CONFIG_AGENT_INVALID",
            "Agent config must be an object.",
            "agent",
        );
        d.check()?;
    }
    let chosen = selection(field(raw, "model"), "agent.model", providers, d)?;
    for (key, allowed, code, msg) in [
        (
            "sandboxMode",
            &["read-only", "workspace-write", "danger-full-access"][..],
            "CONFIG_AGENT_SANDBOX_MODE_INVALID",
            "agent.sandboxMode must be read-only, workspace-write, danger-full-access; using danger-full-access.",
        ),
        (
            "runtimeContextSurface",
            &["system_prompt", "user_message"][..],
            "CONFIG_AGENT_RUNTIME_CONTEXT_SURFACE_INVALID",
            "agent.runtimeContextSurface must be system_prompt or user_message; using the system_prompt profile default.",
        ),
        (
            "interactionProfile",
            &["interactive", "headless", "disabled"][..],
            "CONFIG_AGENT_INTERACTION_PROFILE_INVALID",
            "agent.interactionProfile must be interactive, headless, or disabled; using the interactive profile default.",
        ),
    ] {
        let v = field(raw, key);
        if !nullish(v) && !allowed.iter().any(|s| eq(v, s)) {
            d.warn(code, msg, &format!("agent.{key}"));
        }
    }
    let sub = field(raw, "subagents");
    if !sub.is_undefined() {
        if !record(sub) {
            return Err(Error::new(
                "CONFIG_AGENT_SUBAGENTS_INVALID",
                "agent.subagents must be an object.",
            ));
        }
        for (k, v) in entries(sub) {
            let k = js_string(&JsValue::String(k.clone()));
            if k == "params" && !(record(v) && entries(v).is_empty()) {
                d.warn(
                    "CONFIG_AGENT_SUBAGENTS_PARAMS_UNSUPPORTED",
                    "agent.subagents.params is not supported and will be ignored.",
                    "agent.subagents.params",
                );
            } else if !["timeoutMs", "maxDepth", "default", "params"].contains(&k.as_str()) {
                d.warn(
                    "CONFIG_AGENT_UNKNOWN_FIELD",
                    format!("Unknown agent.subagents field {k}."),
                    &format!("agent.subagents.{k}"),
                );
            }
        }
        let def = field(sub, "default");
        if !nullish(def)
            && !def
                .as_string()
                .is_some_and(|s| trim(s).equals_str("inherit"))
        {
            subagent_ref(def, providers, d);
        }
        immediate_number(
            field(sub, "maxDepth"),
            "agent.subagents.maxDepth",
            "CONFIG_INVALID_VALUE",
            false,
            "non-negative integer",
        )?;
        immediate_number(
            field(sub, "timeoutMs"),
            "agent.subagents.timeoutMs",
            "CONFIG_INVALID_VALUE",
            true,
            "positive integer",
        )?;
    }
    for key in ["maxContextTokens", "maxOutputTokens"] {
        immediate_number(
            field(raw, key),
            &format!("agent.{key}"),
            "CONFIG_INVALID_VALUE",
            true,
            "positive integer",
        )?;
    }
    if !field(raw, "fallbackModel").is_undefined() {
        d.warn("CONFIG_AGENT_FALLBACK_MODEL_DEPRECATED","agent.fallbackModel has been removed. Move the value to router.fallback.default to keep fallback behaviour.","agent.fallbackModel");
    }
    d.check()?;
    Ok(chosen)
}
fn subagent_ref(v: &JsValue, p: &[Provider], d: &mut Diagnostics) {
    let path = "agent.subagents.default";
    let Some(s) = v.as_string().map(trim).filter(|s| !s.is_empty()) else {
        d.warn(
            "CONFIG_AGENT_SUBAGENT_MODEL_INVALID",
            format!(
                "{path} must be inherit or a provider/model string. Inheriting agent.model instead."
            ),
            path,
        );
        return;
    };
    let Some(i) = s
        .units()
        .iter()
        .position(|u| *u == 47)
        .filter(|i| *i > 0 && *i + 1 < s.units().len())
    else {
        d.warn(
            "CONFIG_AGENT_SUBAGENT_MODEL_INVALID",
            format!("{path} must use provider/model format. Inheriting agent.model instead."),
            path,
        );
        return;
    };
    let a = JsString::from_utf16(s.units()[..i].to_vec());
    let b = JsString::from_utf16(s.units()[i + 1..].to_vec());
    let at = js_string(&JsValue::String(a.clone()));
    if let Some(p) = p.iter().find(|p| p.id == a) {
        if !p.models.iter().any(|m| m.id == b) {
            d.warn("CONFIG_AGENT_SUBAGENT_MODEL_NOT_FOUND",format!("{path} references unknown model {} for provider {at}. Inheriting agent.model instead.",js_string(&JsValue::String(b))),path);
        }
    } else {
        d.warn(
            "CONFIG_AGENT_SUBAGENT_PROVIDER_NOT_FOUND",
            format!("{path} references unknown provider {at}. Inheriting agent.model instead."),
            path,
        );
    }
}
pub(super) fn extension(raw: &JsValue, d: &mut Diagnostics) {
    if !shape(
        raw,
        d,
        "CONFIG_EXTENSION_INVALID",
        "extension config must be an object.",
        "extension",
    ) {
        return;
    }
    for (key, valid, code, msg) in [
        (
            "builtinPluginsEnabled",
            record(field(raw, "builtinPluginsEnabled")),
            "CONFIG_EXTENSION_BUILTIN_PLUGINS_INVALID",
            "extension.builtinPluginsEnabled must be an object of booleans.",
        ),
        (
            "includeHookEvents",
            field(raw, "includeHookEvents").as_bool().is_some(),
            "CONFIG_EXTENSION_INCLUDE_HOOK_EVENTS_INVALID",
            "extension.includeHookEvents must be a boolean.",
        ),
    ] {
        if !field(raw, key).is_undefined() && !valid {
            d.fatal(code, msg, &format!("extension.{key}"));
        }
    }
    unknown(
        raw,
        &["builtinPluginsEnabled", "includeHookEvents"],
        d,
        "CONFIG_EXTENSION_UNKNOWN_FIELD",
        "extension",
        "extension config field",
    );
}
fn memory_string(v: &JsValue, path: &str) -> Result<(), Error> {
    if !v.is_undefined() && v.as_string().is_none_or(|s| s.is_empty()) {
        return Err(Error::new(
            "CONFIG_MEMORY_VALUE_INVALID",
            format!("{path} must be a non-empty string."),
        ));
    }
    Ok(())
}
fn memory_bool(v: &JsValue, path: &str) -> Result<(), Error> {
    if !v.is_undefined() && v.as_bool().is_none() {
        return Err(Error::new(
            "CONFIG_MEMORY_VALUE_INVALID",
            format!("{path} must be a boolean."),
        ));
    }
    Ok(())
}
fn memory_mode(v: &JsValue) -> Result<(), Error> {
    if !v.is_undefined() && !eq(v, "answer_first") && !eq(v, "accuracy_first") {
        return Err(Error::new(
            "CONFIG_MEMORY_VALUE_INVALID",
            "memory.schedule.reasoningMode must be answer_first or accuracy_first.",
        ));
    }
    Ok(())
}
pub(super) fn memory(raw: &JsValue, p: &[Provider], d: &mut Diagnostics) -> Result<(), Error> {
    if !shape(
        raw,
        d,
        "CONFIG_MEMORY_INVALID",
        "memory config must be an object.",
        "memory",
    ) {
        return Ok(());
    }
    memory_bool(field(raw, "enabled"), "memory.enabled")?;
    let provider = field(raw, "provider");
    memory_string(provider, "memory.provider")?;
    if !provider.is_undefined() && !eq(provider, "edgeclaw") {
        d.fatal(
            "CONFIG_MEMORY_PROVIDER_UNSUPPORTED",
            format!("Unsupported memory provider {}.", js_string(provider)),
            "memory.provider",
        );
        return Ok(());
    }
    let m = field(raw, "model");
    if !nullish(m) && !m.as_string().is_some_and(|s| trim(s).is_empty()) {
        let Some(s) = m.as_string() else {
            return Err(Error::new(
                "CONFIG_MEMORY_MODEL_INVALID",
                "memory.model must be a \"provider/model\" string.",
            ));
        };
        let Some(i) = s.units().iter().position(|u| *u == 47) else {
            return Err(Error::new(
                "CONFIG_MEMORY_MODEL_INVALID",
                "memory.model must use \"provider/model\" format.",
            ));
        };
        let a = JsString::from_utf16(s.units()[..i].to_vec());
        let b = JsString::from_utf16(s.units()[i + 1..].to_vec());
        let at = js_string(&JsValue::String(a.clone()));
        if let Some(p) = p.iter().find(|p| p.id == a) {
            if !p.models.iter().any(|m| m.id == b) {
                d.warn(
                    "CONFIG_MEMORY_MODEL_NOT_FOUND",
                    format!(
                        "memory.model references unknown model {} for provider {at}.",
                        js_string(&JsValue::String(b))
                    ),
                    "memory.model",
                );
            }
        } else {
            d.warn(
                "CONFIG_MEMORY_MODEL_PROVIDER_NOT_FOUND",
                format!("memory.model references unknown provider {at}."),
                "memory.model",
            );
        }
    }
    let schedule = field(raw, "schedule");
    let mut has_schedule = false;
    if !schedule.is_undefined() {
        if !record(schedule) {
            return Err(Error::new(
                "CONFIG_MEMORY_VALUE_INVALID",
                "memory.schedule must be an object.",
            ));
        }
        memory_mode(field(schedule, "reasoningMode"))?;
        for key in ["autoIndexIntervalMinutes", "autoDreamIntervalMinutes"] {
            immediate_number(
                field(schedule, key),
                &format!("memory.schedule.{key}"),
                "CONFIG_MEMORY_VALUE_INVALID",
                false,
                "non-negative number",
            )?;
        }
        has_schedule = [
            "reasoningMode",
            "autoIndexIntervalMinutes",
            "autoDreamIntervalMinutes",
        ]
        .iter()
        .any(|key| !field(schedule, key).is_undefined());
        unknown(
            schedule,
            &[
                "reasoningMode",
                "autoIndexIntervalMinutes",
                "autoDreamIntervalMinutes",
            ],
            d,
            "CONFIG_MEMORY_UNKNOWN_FIELD",
            "memory.schedule",
            "memory.schedule field",
        );
    }
    if !has_schedule {
        memory_mode(field(raw, "reasoningMode"))?;
    }
    unknown(
        raw,
        &[
            "enabled",
            "provider",
            "rootDir",
            "captureStrategy",
            "includeAssistant",
            "maxMessageChars",
            "retrievalTimeoutMs",
            "model",
            "apiType",
            "schedule",
            "heartbeatBatchSize",
            "reasoningMode",
            "autoIndexIntervalMinutes",
            "autoDreamIntervalMinutes",
        ],
        d,
        "CONFIG_MEMORY_UNKNOWN_FIELD",
        "memory",
        "memory field",
    );
    memory_string(field(raw, "rootDir"), "memory.rootDir")?;
    let capture = field(raw, "captureStrategy");
    if !capture.is_undefined() && !eq(capture, "last_turn") && !eq(capture, "full_session") {
        return Err(Error::new(
            "CONFIG_MEMORY_CAPTURE_INVALID",
            "memory.captureStrategy must be last_turn or full_session.",
        ));
    }
    memory_bool(field(raw, "includeAssistant"), "memory.includeAssistant")?;
    for key in ["maxMessageChars", "retrievalTimeoutMs"] {
        immediate_number(
            field(raw, key),
            &format!("memory.{key}"),
            "CONFIG_MEMORY_VALUE_INVALID",
            true,
            "positive number",
        )?;
    }
    let api = field(raw, "apiType");
    if !api.is_undefined()
        && ![
            "openai-responses",
            "responses",
            "openai-completions",
            "anthropic",
            "google",
        ]
        .iter()
        .any(|s| eq(api, s))
    {
        return Err(Error::new(
            "CONFIG_MEMORY_VALUE_INVALID",
            "memory.apiType must be openai-responses, responses, openai-completions, anthropic, or google.",
        ));
    }
    immediate_number(
        field(raw, "heartbeatBatchSize"),
        "memory.heartbeatBatchSize",
        "CONFIG_MEMORY_VALUE_INVALID",
        true,
        "positive number",
    )?;
    Ok(())
}
pub(super) fn gateway(raw: &JsValue, d: &mut Diagnostics) {
    if !shape(
        raw,
        d,
        "GATEWAY_CONFIG_INVALID",
        "gateway config must be an object.",
        "gateway",
    ) {
        return;
    }
    let bind = field(raw, "bindAddress");
    if bind.as_string().is_some() && !eq(bind, "127.0.0.1") {
        d.fatal(
            "GATEWAY_BIND_ADDRESS_UNSUPPORTED",
            "gateway.bindAddress must be 127.0.0.1 in the first phase.",
            "gateway.bindAddress",
        );
    }
    if !field(raw, "tokenPath").is_undefined() {
        d.warn("GATEWAY_TOKEN_PATH_REMOVED","gateway.tokenPath is no longer configurable; the gateway token is stored under PilotHome.","gateway.tokenPath");
    }
}
pub(super) fn adapters(raw: &JsValue, d: &mut Diagnostics) {
    shape(
        raw,
        d,
        "ADAPTERS_CONFIG_INVALID",
        "adapters config must be an object.",
        "adapters",
    );
}
pub(super) fn always_on(raw: &JsValue, d: &mut Diagnostics) -> Result<(), Error> {
    if !shape(
        raw,
        d,
        "ALWAYS_ON_CONFIG_INVALID",
        "alwaysOn config must be an object.",
        "alwaysOn",
    ) {
        return Ok(());
    }
    if !field(raw, "enabled").is_undefined() {
        d.warn("ALWAYS_ON_FIELD_REMOVED","alwaysOn.enabled has been removed. Always-On is active when any project in alwaysOn.projects is enabled; this field is ignored.","alwaysOn.enabled");
    }
    let language = field(raw, "language");
    if !string_coercible(language) {
        return Err(primitive_error());
    }
    if !language.is_undefined() && !eq(language, "en") && !eq(language, "zh-CN") {
        d.warn(
            "ALWAYS_ON_LANGUAGE_INVALID",
            format!(
                "alwaysOn.language must be \"en\" or \"zh-CN\"; ignoring \"{}\".",
                js_string(language)
            ),
            "alwaysOn.language",
        );
    }
    for (k, _) in entries(raw) {
        if builtin_property(k) {
            inherited_removal(d, k, "alwaysOn");
            continue;
        }
        let k = js_string(&JsValue::String(k.clone()));
        let removed = match k.as_str() {
            "discovery" => Some(
                "alwaysOn.discovery wrapper has been removed. Lift trigger / projects to alwaysOn.<key>.",
            ),
            "plan" => Some(
                "alwaysOn.plan section has been removed. plan-per-fire is fixed at 1 by protocol.",
            ),
            "cron" => Some("Always-On cron is no longer part of this module."),
            "dormancy" => Some(
                "alwaysOn.dormancy has been removed. Dormancy is always active with built-in defaults and can no longer be configured.",
            ),
            "workspace" => Some(
                "alwaysOn.workspace has been removed. Workspace parameters are now hardcoded internally.",
            ),
            "execution" => Some(
                "alwaysOn.execution has been removed. Execution limits are now managed by the runtime.",
            ),
            _ => None,
        };
        if let Some(msg) = removed {
            d.warn("ALWAYS_ON_FIELD_REMOVED", msg, &format!("alwaysOn.{k}"));
        } else if !["enabled", "language", "trigger", "projects"].contains(&k.as_str()) {
            d.warn(
                "ALWAYS_ON_UNKNOWN_FIELD",
                format!("Unknown alwaysOn field {k}."),
                &format!("alwaysOn.{k}"),
            );
        }
    }
    let trigger = field(raw, "trigger");
    if shape(
        trigger,
        d,
        "ALWAYS_ON_TRIGGER_INVALID",
        "alwaysOn.trigger must be an object.",
        "alwaysOn.trigger",
    ) {
        if !field(trigger, "enabled").is_undefined() {
            d.warn(
                "ALWAYS_ON_FIELD_REMOVED",
                "alwaysOn.trigger.enabled has been removed and is ignored.",
                "alwaysOn.trigger.enabled",
            );
        }
        for (key, fallback, positive, integer, label) in [
            ("tickIntervalMinutes", 5, true, false, "positive number"),
            ("cooldownMinutes", 60, false, false, "non-negative number"),
            ("dailyBudget", 4, false, true, "non-negative integer"),
            ("heartbeatStaleSeconds", 90, true, false, "positive number"),
            (
                "recentUserMsgMinutes",
                5,
                false,
                false,
                "non-negative number",
            ),
        ] {
            let v = field(trigger, key);
            if !v.is_undefined() && !number(v, positive, integer) {
                let path = format!("alwaysOn.trigger.{key}");
                d.warn(
                    "ALWAYS_ON_NUMBER_INVALID",
                    format!("{path} must be a {label}; falling back to {fallback}."),
                    &path,
                );
            }
        }
        let pref = field(trigger, "preferChannel");
        if !pref.is_undefined() && pref.as_string().is_none_or(|s| s.is_empty()) {
            d.warn("ALWAYS_ON_TRIGGER_PREFER_CHANNEL_INVALID","alwaysOn.trigger.preferChannel must be a non-empty string; falling back to default.","alwaysOn.trigger.preferChannel");
        }
    }
    let projects = field(raw, "projects");
    if shape(
        projects,
        d,
        "ALWAYS_ON_PROJECTS_INVALID",
        "alwaysOn.projects must be an object keyed by absolute project root.",
        "alwaysOn.projects",
    ) {
        for (root, v) in entries(projects) {
            if trim(root).is_empty() {
                continue;
            }
            let root = js_string(&JsValue::String(root.clone()));
            let path = format!("alwaysOn.projects.{root}");
            if !record(v) {
                d.fatal(
                    "ALWAYS_ON_PROJECT_INVALID",
                    format!("{path} must be an object."),
                    &path,
                );
                continue;
            }
            for (k, _) in entries(v) {
                if builtin_property(k) {
                    inherited_removal(d, k, &path);
                    continue;
                }
                let k = js_string(&JsValue::String(k.clone()));
                let msg = match k.as_str() {
                    "sessionKey" => Some(
                        "alwaysOn.projects.<root>.sessionKey is no longer accepted. The runtime derives sessionKey from projectKey + runId.",
                    ),
                    "workspace" => Some(
                        "alwaysOn.projects.<root>.workspace per-project override is no longer accepted. WorkspaceProviderRegistry resolves provider automatically.",
                    ),
                    _ => None,
                };
                if let Some(msg) = msg {
                    d.warn("ALWAYS_ON_FIELD_REMOVED", msg, &format!("{path}.{k}"));
                } else if k != "enabled" {
                    d.warn(
                        "ALWAYS_ON_PROJECT_UNKNOWN_FIELD",
                        format!("Unknown {path}.{k}; only 'enabled' is accepted."),
                        &format!("{path}.{k}"),
                    );
                }
            }
        }
    }
    Ok(())
}
fn valid_timezone(s: &str) -> bool {
    // Node 22 accepts case-insensitive ICU zone IDs plus UTC offset identifiers.
    static ZONES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let zones = ZONES.get_or_init(|| {
        serde_json::from_str(include_str!("timezones.json")).expect("recorded ICU zones")
    });
    if s.is_ascii() && zones.binary_search(&s.to_lowercase()).is_ok() {
        return true;
    }
    let re = regex::Regex::new(r"^[+-]([0-9]{2})(?::?([0-9]{2}))?$").unwrap();
    re.captures(s).is_some_and(|c| {
        c[1].parse::<u32>().unwrap() < 24
            && c.get(2)
                .is_none_or(|m| m.as_str().parse::<u32>().unwrap() < 60)
    })
}
pub(super) fn cron(raw: &JsValue, d: &mut Diagnostics) {
    if !shape(
        raw,
        d,
        "CRON_CONFIG_INVALID",
        "cron config must be an object.",
        "cron",
    ) {
        return;
    }
    let v = field(raw, "timezone");
    let zone = if v.is_undefined() {
        "UTC".into()
    } else if let Some(s) = nonempty(v) {
        s
    } else {
        d.warn(
            "CRON_STRING_INVALID",
            "cron.timezone must be a non-empty string; falling back to UTC.",
            "cron.timezone",
        );
        "UTC".into()
    };
    if !valid_timezone(&zone) {
        d.warn(
            "CRON_TIMEZONE_INVALID",
            "cron.timezone must be a valid IANA timezone; falling back to UTC.",
            "cron.timezone",
        );
    }
    for (key, fallback) in [("maxConcurrentRuns", 1), ("runTimeoutMinutes", 60)] {
        let v = field(raw, key);
        if !v.is_undefined() && !number(v, true, true) {
            let path = format!("cron.{key}");
            d.warn(
                "CRON_NUMBER_INVALID",
                format!("{path} must be a positive integer; falling back to {fallback}."),
                &path,
            );
        }
    }
    unknown(
        raw,
        &[
            "enabled",
            "timezone",
            "maxConcurrentRuns",
            "runTimeoutMinutes",
        ],
        d,
        "CRON_UNKNOWN_FIELD",
        "cron",
        "cron config field",
    );
}
fn enum_field(v: &JsValue, allowed: &[&str], path: &str, code: &str, d: &mut Diagnostics) {
    if !v.is_undefined() && !allowed.iter().any(|s| eq(v, s)) {
        d.fatal(
            code,
            format!("{path} must be one of: {}.", allowed.join(", ")),
            path,
        );
    }
}
pub(super) fn tools(raw: &JsValue, d: &mut Diagnostics) {
    if !shape(
        raw,
        d,
        "TOOLS_CONFIG_INVALID",
        "tools config must be an object.",
        "tools",
    ) {
        return;
    }
    let web = field(raw, "webSearch");
    if shape(
        web,
        d,
        "TOOLS_WEB_SEARCH_INVALID",
        "tools.webSearch must be an object.",
        "tools.webSearch",
    ) {
        let enabled = field(web, "enabled");
        if !enabled.is_undefined() && enabled.as_bool().is_none() {
            d.fatal(
                "TOOLS_WEB_SEARCH_ENABLED_INVALID",
                "tools.webSearch.enabled must be a boolean.",
                "tools.webSearch.enabled",
            );
        }
        if enabled.as_bool() != Some(false) {
            let provider = field(web, "provider");
            if !provider.is_undefined()
                && ![
                    "glm", "tavily", "serper", "brave", "baidu", "bocha", "exa", "serpapi",
                    "custom",
                ]
                .iter()
                .any(|s| eq(provider, s))
            {
                d.fatal(
                    "TOOLS_WEB_SEARCH_PROVIDER_INVALID",
                    "tools.webSearch.provider must be a supported search provider.",
                    "tools.webSearch.provider",
                );
            }
            enum_field(
                field(web, "searchEngine"),
                &["google", "bing", "baidu", "duckduckgo", "yahoo", "yandex"],
                "tools.webSearch.searchEngine",
                "TOOLS_WEB_SEARCH_ENGINE_INVALID",
                d,
            );
            let key = field(web, "apiKey");
            if !key.is_undefined() && key.as_string().is_none_or(|s| trim(s).is_empty()) {
                d.fatal(
                    "TOOLS_WEB_SEARCH_API_KEY_INVALID",
                    "tools.webSearch.apiKey must be a non-empty string.",
                    "tools.webSearch.apiKey",
                );
            }
            let ep = field(web, "endpoint");
            if !ep.is_undefined() {
                if let Some(s) = nonempty(ep) {
                    if http_url(&s).is_none() {
                        d.fatal(
                            "TOOLS_WEB_SEARCH_ENDPOINT_INVALID",
                            "tools.webSearch.endpoint must be an HTTP(S) URL.",
                            "tools.webSearch.endpoint",
                        );
                    }
                } else {
                    d.fatal(
                        "TOOLS_WEB_SEARCH_ENDPOINT_INVALID",
                        "tools.webSearch.endpoint must be a non-empty URL string.",
                        "tools.webSearch.endpoint",
                    );
                }
            }
            let custom = field(web, "customProvider");
            if shape(
                custom,
                d,
                "TOOLS_WEB_SEARCH_CUSTOM_PROVIDER_INVALID",
                "tools.webSearch.customProvider must be an object.",
                "tools.webSearch.customProvider",
            ) {
                enum_field(
                    field(custom, "auth"),
                    &["bearer", "bodyApiKey", "queryApiKey", "none"],
                    "tools.webSearch.customProvider.auth",
                    "TOOLS_WEB_SEARCH_CUSTOM_AUTH_INVALID",
                    d,
                );
                enum_field(
                    field(custom, "method"),
                    &["GET", "POST"],
                    "tools.webSearch.customProvider.method",
                    "TOOLS_WEB_SEARCH_CUSTOM_METHOD_INVALID",
                    d,
                );
                let fields = [
                    "name",
                    "queryParam",
                    "apiKeyParam",
                    "resultsPath",
                    "titleField",
                    "urlField",
                    "snippetField",
                    "sourceField",
                    "publishedAtField",
                ];
                for key in fields {
                    let v = field(custom, key);
                    if !v.is_undefined() && v.as_string().is_none_or(|s| trim(s).is_empty()) {
                        let path = format!("tools.webSearch.customProvider.{key}");
                        d.fatal(
                            "TOOLS_WEB_SEARCH_CUSTOM_STRING_INVALID",
                            format!("{path} must be a non-empty string."),
                            &path,
                        );
                    }
                }
                unknown(
                    custom,
                    &[
                        "auth",
                        "method",
                        "name",
                        "queryParam",
                        "apiKeyParam",
                        "resultsPath",
                        "titleField",
                        "urlField",
                        "snippetField",
                        "sourceField",
                        "publishedAtField",
                    ],
                    d,
                    "TOOLS_WEB_SEARCH_CUSTOM_PROVIDER_UNKNOWN_FIELD",
                    "tools.webSearch.customProvider",
                    "tools.webSearch.customProvider field",
                );
            }
            for (key, code, msg) in [
                (
                    "region",
                    "TOOLS_WEB_SEARCH_REGION_DEPRECATED",
                    "tools.webSearch.region has been removed. Select tools.webSearch.provider instead.",
                ),
                (
                    "tavilyApiKey",
                    "TOOLS_WEB_SEARCH_TAVILY_KEY_DEPRECATED",
                    "tools.webSearch.tavilyApiKey has been removed. Set tools.webSearch.provider: tavily and use tools.webSearch.apiKey.",
                ),
            ] {
                if !field(web, key).is_undefined() {
                    d.warn(code, msg, &format!("tools.webSearch.{key}"));
                }
            }
            unknown(
                web,
                &[
                    "enabled",
                    "provider",
                    "apiKey",
                    "endpoint",
                    "customProvider",
                    "searchEngine",
                    "region",
                    "tavilyApiKey",
                ],
                d,
                "TOOLS_WEB_SEARCH_UNKNOWN_FIELD",
                "tools.webSearch",
                "tools.webSearch field",
            );
        }
    }
    unknown(
        raw,
        &["webSearch"],
        d,
        "TOOLS_UNKNOWN_FIELD",
        "tools",
        "tools config field",
    );
}
pub(super) fn run_policy(raw: &JsValue, d: &mut Diagnostics) {
    if !shape(
        raw,
        d,
        "RUN_POLICY_INVALID",
        "runPolicy config must be an object.",
        "runPolicy",
    ) {
        return;
    }
    let fg = field(raw, "failureGuard");
    if shape(
        fg,
        d,
        "RUN_POLICY_FAILURE_GUARD_INVALID",
        "runPolicy.failureGuard must be an object.",
        "runPolicy.failureGuard",
    ) {
        let enabled = field(fg, "enabled");
        if !enabled.is_undefined() && enabled.as_bool().is_none() {
            d.fatal(
                "RUN_POLICY_FAILURE_GUARD_ENABLED_INVALID",
                "runPolicy.failureGuard.enabled must be a boolean.",
                "runPolicy.failureGuard.enabled",
            );
        }
        let limit = field(fg, "modelFailureLimit");
        if !limit.is_undefined() && !number(limit, false, true) {
            d.fatal(
                "RUN_POLICY_FAILURE_GUARD_MODEL_LIMIT_INVALID",
                "runPolicy.failureGuard.modelFailureLimit must be a non-negative integer.",
                "runPolicy.failureGuard.modelFailureLimit",
            );
        }
        let regex = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,199}$").unwrap();
        for (key, root_code, key_code, value_code, label) in [
            (
                "toolFailureLimits",
                "RUN_POLICY_FAILURE_GUARD_TOOL_LIMITS_INVALID",
                "RUN_POLICY_FAILURE_GUARD_TOOL_LIMIT_KEY_INVALID",
                "RUN_POLICY_FAILURE_GUARD_TOOL_LIMIT_INVALID",
                false,
            ),
            (
                "toolLabels",
                "RUN_POLICY_FAILURE_GUARD_TOOL_LABELS_INVALID",
                "RUN_POLICY_FAILURE_GUARD_TOOL_LABEL_KEY_INVALID",
                "RUN_POLICY_FAILURE_GUARD_TOOL_LABEL_INVALID",
                true,
            ),
        ] {
            let path = format!("runPolicy.failureGuard.{key}");
            let v = field(fg, key);
            if shape(
                v,
                d,
                root_code,
                &format!("{path} must be an object."),
                &path,
            ) {
                for (k, v) in entries(v) {
                    let k = js_string(&JsValue::String(k.clone()));
                    if !regex.is_match(&k) {
                        d.fatal(
                            key_code,
                            format!("{path} has an invalid key."),
                            &format!("{path}.{k}"),
                        );
                        continue;
                    }
                    if if label {
                        v.as_string().is_none_or(|s| trim(s).is_empty())
                    } else {
                        !number(v, false, true)
                    } {
                        d.fatal(
                            value_code,
                            format!(
                                "{path}.{k} must be a {}.",
                                if label {
                                    "non-empty string"
                                } else {
                                    "non-negative integer"
                                }
                            ),
                            &format!("{path}.{k}"),
                        );
                    }
                }
            }
        }
        unknown(
            fg,
            &[
                "enabled",
                "modelFailureLimit",
                "toolFailureLimits",
                "toolLabels",
            ],
            d,
            "RUN_POLICY_FAILURE_GUARD_UNKNOWN_FIELD",
            "runPolicy.failureGuard",
            "runPolicy.failureGuard field",
        );
    }
    unknown(
        raw,
        &["failureGuard"],
        d,
        "RUN_POLICY_UNKNOWN_FIELD",
        "runPolicy",
        "runPolicy config field",
    );
}
pub(super) fn proxy(raw: &JsValue, d: &mut Diagnostics) {
    let mut proxy = field(raw, "proxy").clone();
    let legacy = field(field(field(raw, "webui"), "runtime"), "httpsProxy");
    if proxy.is_undefined()
        && record(field(raw, "webui"))
        && record(field(field(raw, "webui"), "runtime"))
        && legacy.as_string().is_some_and(|s| !s.is_empty())
    {
        proxy = crate::metadata::object([("url", legacy.clone())]);
        d.add("CONFIG_PROXY_MIGRATED","info","webui.runtime.httpsProxy has been migrated to the top-level proxy.url field. Please update your pilotdeck.yaml to use proxy.url instead.","webui.runtime.httpsProxy");
    }
    if proxy.is_undefined() || proxy.as_string().is_some() {
        return;
    }
    if !record(&proxy) {
        d.warn(
            "CONFIG_PROXY_INVALID",
            "proxy must be a string or an object with a url field.",
            "proxy",
        );
        return;
    }
    if field(&proxy, "url")
        .as_string()
        .is_none_or(|s| s.is_empty())
    {
        d.warn(
            "CONFIG_PROXY_URL_MISSING",
            "proxy.url must be a non-empty string.",
            "proxy.url",
        );
    }
}

fn inherited_removal(d: &mut Diagnostics, key: &JsString, path: &str) {
    let name = js_string(&JsValue::String(key.clone()));
    d.warn("ALWAYS_ON_FIELD_REMOVED", "", &format!("{path}.{name}"));
    if let Some(JsValue::Object(last)) = d.0.last_mut() {
        last.insert(
            "message".into(),
            if key.equals_str("__proto__") {
                crate::metadata::object([])
            } else {
                JsValue::Undefined
            },
        );
    }
}
