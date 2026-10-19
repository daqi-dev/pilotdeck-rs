//! Complete executable configuration snapshot, mirroring the fixed TypeScript
//! baseline's `src/pilot/config/loadPilotConfig.ts` output shape.
//!
//! The existing validators decide whether a load succeeds and which diagnostics
//! are emitted. This module only projects the already-validated raw document
//! into the snapshot's normalized value shape; it never changes validation.
use super::{
    Env,
    model::Provider,
    sections::{tool_key_regex, valid_timezone},
    value::*,
};
use crate::metadata::{Files, Metadata, object};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[derive(Clone, Debug)]
pub(crate) struct SourceRecord {
    kind: String,
    priority: f64,
    phase: Option<&'static str>,
    path: Option<String>,
    content_hash: String,
}

impl SourceRecord {
    pub(crate) fn bootstrap_env() -> Self {
        Self {
            kind: "env".into(),
            priority: 30.0,
            phase: Some("bootstrap"),
            path: None,
            content_hash: sha256_text("PILOT_HOME=<redacted-path>"),
        }
    }
    pub(crate) fn merge_env(env_config: &JsValue) -> Self {
        let redacted = redact(env_config);
        let stable = stable_stringify(&redacted).unwrap_or_default();
        Self {
            kind: "env".into(),
            priority: 30.0,
            phase: Some("merge"),
            path: None,
            content_hash: sha256_text(&stable),
        }
    }
    pub(crate) fn file(kind: &str, priority: f64, path: &str, content: &JsString) -> Self {
        Self {
            kind: kind.to_owned(),
            priority,
            phase: None,
            path: Some(path.to_owned()),
            content_hash: sha256_content(content),
        }
    }
    fn value(&self, loaded_at: &JsString) -> JsValue {
        let mut o = JsObject::new();
        o.insert("kind".into(), self.kind.as_str().into());
        if let Some(phase) = self.phase {
            o.insert("phase".into(), phase.into());
        }
        o.insert("priority".into(), JsValue::Number(self.priority));
        o.insert("loadedAt".into(), JsValue::String(loaded_at.clone()));
        if let Some(path) = &self.path {
            o.insert("path".into(), path.as_str().into());
        }
        o.insert("contentHash".into(), self.content_hash.as_str().into());
        JsValue::Object(o)
    }
}

/// Build the snapshot object after the validators have accepted the document.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build<F: Files>(
    metadata: &Metadata<F>,
    env: &Env,
    raw: &JsValue,
    providers: &[Provider],
    selection: &(JsString, JsString),
    sources: Vec<SourceRecord>,
    version: f64,
    loaded_at: &JsString,
    diagnostics: &[JsValue],
) -> JsValue {
    let agent = agent(raw, providers, selection);
    let model = model(raw, providers, env);
    let extension = extension(raw);
    let memory = memory(raw, metadata.home.as_path());
    let gateway = gateway(raw);
    let adapters = adapters(raw);
    let router = router(raw, providers, selection);
    let always_on = always_on(raw, metadata.cwd.as_path());
    let cron = cron(raw);
    let tools = tools(raw);
    let telemetry = telemetry(raw);
    let proxy = proxy(raw);
    let modules = modules(raw, metadata.home.as_path(), env);
    let run_policy = run_policy(raw);

    let mut config = JsObject::new();
    config.insert("agent".into(), agent);
    config.insert("model".into(), model);
    config.insert("extension".into(), extension);
    if let Some(v) = memory {
        config.insert("memory".into(), v);
    }
    if let Some(v) = gateway {
        config.insert("gateway".into(), v);
    }
    if let Some(v) = adapters {
        config.insert("adapters".into(), v);
    }
    if let Some(v) = router {
        config.insert("router".into(), v);
    }
    if let Some(v) = always_on {
        config.insert("alwaysOn".into(), v);
    }
    if let Some(v) = cron {
        config.insert("cron".into(), v);
    }
    if let Some(v) = tools {
        config.insert("tools".into(), v);
    }
    config.insert("telemetry".into(), telemetry);
    if let Some(v) = proxy {
        config.insert("proxy".into(), v);
    }
    if let Some(v) = modules {
        config.insert("modules".into(), v);
    }
    if let Some(v) = run_policy {
        config.insert("runPolicy".into(), v);
    }
    let config = JsValue::Object(config);
    let content_hash = content_hash(&config);

    let mut snapshot = JsObject::new();
    snapshot.insert("version".into(), JsValue::Number(version));
    snapshot.insert("schemaVersion".into(), JsValue::Number(1.0));
    snapshot.insert("loadedAt".into(), JsValue::String(loaded_at.clone()));
    snapshot.insert("contentHash".into(), content_hash.as_str().into());
    snapshot.insert(
        "sources".into(),
        JsValue::Array(sources.iter().map(|s| s.value(loaded_at)).collect()),
    );
    snapshot.insert(
        "diagnostics".into(),
        JsValue::Array(diagnostics.iter().map(snapshot_diagnostic).collect()),
    );
    snapshot.insert("config".into(), config);
    js_order(&JsValue::Object(snapshot))
}

fn snapshot_diagnostic(value: &JsValue) -> JsValue {
    let JsValue::Object(object) = value else {
        return value.clone();
    };
    let router_issue = object
        .get("code")
        .and_then(|v| v.as_string())
        .is_some_and(|code| {
            code.units()
                .starts_with("ROUTER_".encode_utf16().collect::<Vec<_>>().as_slice())
        });
    // loadPilotConfig builds diagnostics in this order: code, severity, message,
    // path, [source], [hint], recoverable. Router issues always carry `hint`,
    // even when its value is undefined.
    let mut out = JsObject::new();
    for key in ["code", "severity", "message", "path", "source"] {
        if let Some(v) = object.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if router_issue || object.get("hint").is_some() {
        out.insert(
            "hint".into(),
            object.get("hint").cloned().unwrap_or(JsValue::Undefined),
        );
    }
    if let Some(v) = object.get("recoverable") {
        out.insert("recoverable".into(), v.clone());
    }
    JsValue::Object(out)
}

/// Content hash over the executable config, computed exactly like the
/// baseline's `redactedSnapshotConfig` object literal (absent sections are
/// still present as `undefined`).
pub fn content_hash(config: &JsValue) -> String {
    const SECTIONS: &[&str] = &[
        "agent",
        "model",
        "extension",
        "memory",
        "gateway",
        "adapters",
        "router",
        "alwaysOn",
        "cron",
        "tools",
        "telemetry",
        "proxy",
        "modules",
        "runPolicy",
    ];
    let mut hash_config = JsObject::new();
    for key in SECTIONS {
        hash_config.insert(
            (*key).into(),
            config.get(key).cloned().unwrap_or_else(undefined),
        );
    }
    let redacted = redact(&JsValue::Object(hash_config));
    sha256_text(&stable_stringify(&redacted).unwrap_or_default())
}

fn obj() -> JsObject {
    JsObject::new()
}
fn put(o: &mut JsObject, key: &str, value: JsValue) {
    o.insert(key.into(), value);
}
/// Assignment to a plain object literal does not create an own `__proto__`
/// property; it changes the prototype. Snapshot JSON only observes own keys.
fn put_key(o: &mut JsObject, key: JsString, value: JsValue) {
    if !key.equals_str("__proto__") {
        o.insert(key, value);
    }
}
/// `Object.entries` order: canonical array-index keys ascending first, then the
/// remaining own keys in insertion order.
fn js_entries(object: &JsObject) -> Vec<(&JsString, &JsValue)> {
    let mut entries: Vec<_> = object.entries().iter().map(|(k, v)| (k, v)).collect();
    entries.sort_by_key(|(key, _)| key.array_index().map_or((1, 0), |n| (0, n)));
    entries
}
/// Rebuild a value with JavaScript property enumeration order. JSON encoding
/// and deep equality in the fixture recorder both follow `Object.keys`, which
/// moves canonical array-index keys before the other keys.
fn js_order(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(js_order).collect()),
        JsValue::Object(object) => {
            let mut out = JsObject::new();
            for (key, value) in js_entries(object) {
                out.insert(key.clone(), js_order(value));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}
fn undefined() -> JsValue {
    JsValue::Undefined
}
fn num(n: f64) -> JsValue {
    JsValue::Number(n)
}
fn string(s: &str) -> JsValue {
    JsValue::String(s.into())
}
fn finite(v: &JsValue) -> Option<f64> {
    v.as_number().filter(|n| n.is_finite())
}
fn floor_positive(v: &JsValue) -> Option<f64> {
    match v.as_number() {
        Some(n) if n.is_finite() && n > 0.0 => Some(n.floor()),
        _ => None,
    }
}
fn floor_non_negative(v: &JsValue) -> Option<f64> {
    match v.as_number() {
        Some(n) if n.is_finite() && n >= 0.0 => Some(n.floor()),
        _ => None,
    }
}
fn bool_field(v: &JsValue, fallback: bool) -> bool {
    v.as_bool().unwrap_or(fallback)
}

fn sha256_text(value: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(value.as_bytes());
    format!("{:x}", hash.finalize())
}
fn sha256_content(value: &JsString) -> String {
    sha256_text(&String::from_utf16_lossy(value.units()))
}

/// `redactConfig` from the baseline: secret-looking keys are replaced before
/// hashing, including the `credentialSource` metadata field.
fn redact(value: &JsValue) -> JsValue {
    fn secret(key: &JsString) -> bool {
        let lower = String::from_utf16_lossy(key.units()).to_lowercase();
        [
            "apikey",
            "authorization",
            "cookie",
            "token",
            "secret",
            "password",
            "credential",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    }
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(redact).collect()),
        JsValue::Object(o) => {
            let mut out = JsObject::new();
            for (key, value) in o.entries() {
                // `output[key] = ...` on a plain object cannot create an own
                // `__proto__` key; it only rewrites the prototype.
                if key.equals_str("__proto__") {
                    continue;
                }
                if secret(key) {
                    out.insert(key.clone(), string("<redacted>"));
                } else {
                    out.insert(key.clone(), redact(value));
                }
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

/// `stableStringify` from the baseline. Object keys are sorted with
/// `localeCompare`; for the ASCII configuration vocabulary this is the en-US
/// ICU collation implemented here. `undefined` participates as the literal
/// string rather than being dropped by JSON encoding.
fn stable_stringify(value: &JsValue) -> Option<String> {
    match value {
        JsValue::Undefined => Some("undefined".into()),
        JsValue::Array(items) => {
            let parts: Vec<_> = items
                .iter()
                .map(|v| stable_stringify(v).unwrap_or_default())
                .collect();
            Some(format!("[{}]", parts.join(",")))
        }
        JsValue::Object(o) => {
            let mut entries: Vec<_> = o.entries().iter().collect();
            entries.sort_by(|a, b| locale_compare(&a.0, &b.0));
            let parts: Vec<_> = entries
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        json_string(key),
                        stable_stringify(value).unwrap_or_default()
                    )
                })
                .collect();
            Some(format!("{{{}}}", parts.join(",")))
        }
        other => pilotdeck_protocol::stringify_json(other).ok().flatten(),
    }
}

fn locale_compare(a: &JsString, b: &JsString) -> std::cmp::Ordering {
    static COLLATOR: OnceLock<icu_collator::CollatorBorrowed<'static>> = OnceLock::new();
    let collator = COLLATOR.get_or_init(|| {
        let locale: icu_locale_core::Locale = "en-US".parse().expect("valid locale");
        icu_collator::Collator::try_new(locale.into(), Default::default())
            .expect("bundled collation data")
    });
    collator.compare_utf16(a.units(), b.units())
}

fn json_string(value: &JsString) -> String {
    pilotdeck_protocol::stringify_json(&JsValue::String(value.clone()))
        .ok()
        .flatten()
        .unwrap_or_else(|| "\"\"".into())
}

// ---------------------------------------------------------------------------
// agent / extension
// ---------------------------------------------------------------------------

fn agent(raw: &JsValue, providers: &[Provider], selection: &(JsString, JsString)) -> JsValue {
    let a = field(raw, "agent");
    let mut out = obj();
    let mut model = obj();
    let id = field(a, "model")
        .as_string()
        .cloned()
        .unwrap_or_else(|| selection_id(selection));
    put(&mut model, "id", JsValue::String(id));
    put(&mut model, "provider", JsValue::String(selection.0.clone()));
    put(&mut model, "model", JsValue::String(selection.1.clone()));
    put(&mut out, "model", JsValue::Object(model));
    let sandbox = field(a, "sandboxMode");
    put(
        &mut out,
        "sandboxMode",
        if eq(sandbox, "read-only")
            || eq(sandbox, "workspace-write")
            || eq(sandbox, "danger-full-access")
        {
            sandbox.clone()
        } else {
            string("danger-full-access")
        },
    );
    let surface = field(a, "runtimeContextSurface");
    put(
        &mut out,
        "runtimeContextSurface",
        if eq(surface, "system_prompt") || eq(surface, "user_message") {
            surface.clone()
        } else {
            string("system_prompt")
        },
    );
    let profile = field(a, "interactionProfile");
    put(
        &mut out,
        "interactionProfile",
        if eq(profile, "interactive") || eq(profile, "headless") || eq(profile, "disabled") {
            profile.clone()
        } else {
            string("interactive")
        },
    );
    for key in ["maxContextTokens", "maxOutputTokens"] {
        if let Some(n) = floor_positive(field(a, key)) {
            put(&mut out, key, num(n));
        }
    }
    let thinking = field(a, "thinking");
    if let Some(t) = thinking.as_object()
        && field(&JsValue::Object(t.clone()), "enabled").as_bool() == Some(true)
    {
        let mut value = obj();
        put(&mut value, "enabled", JsValue::Bool(true));
        if let Some(n) = field(&JsValue::Object(t.clone()), "budgetTokens").as_number() {
            put(&mut value, "budgetTokens", num(n));
        }
        put(&mut out, "thinking", JsValue::Object(value));
    }
    let sub = field(a, "subagents");
    if let Some(s) = sub.as_object() {
        let s = JsValue::Object(s.clone());
        let mut value = obj();
        let default = field(&s, "default");
        if !nullish(default)
            && !(default
                .as_string()
                .is_some_and(|v| trim(v).equals_str("inherit")))
            && let Some((provider, model)) = split_ref(default)
            && providers
                .iter()
                .any(|p| p.id == provider && p.models.iter().any(|m| m.id == model))
        {
            let mut d = obj();
            let id = default.as_string().map(trim).unwrap_or_default();
            put(&mut d, "id", JsValue::String(id));
            put(&mut d, "provider", JsValue::String(provider));
            put(&mut d, "model", JsValue::String(model));
            put(&mut value, "default", JsValue::Object(d));
        }
        put(
            &mut value,
            "timeoutMs",
            floor_positive(field(&s, "timeoutMs"))
                .map(num)
                .unwrap_or_else(undefined),
        );
        put(
            &mut value,
            "maxDepth",
            floor_non_negative(field(&s, "maxDepth"))
                .map(num)
                .unwrap_or_else(undefined),
        );
        put(&mut out, "subagents", JsValue::Object(value));
    }
    JsValue::Object(out)
}

fn split_ref(v: &JsValue) -> Option<(JsString, JsString)> {
    let s = trim(v.as_string()?);
    let i = s.units().iter().position(|u| *u == 47)?;
    if i == 0 || i + 1 >= s.units().len() {
        return None;
    }
    Some((
        JsString::from_utf16(s.units()[..i].to_vec()),
        JsString::from_utf16(s.units()[i + 1..].to_vec()),
    ))
}

fn extension(raw: &JsValue) -> JsValue {
    let mut out = obj();
    let mut plugins = obj();
    let mut hook = JsValue::Bool(false);
    if let Some(e) = field(raw, "extension").as_object() {
        if let Some(b) = e.get("builtinPluginsEnabled").and_then(|v| v.as_object()) {
            // loadPilotConfig uses Object.fromEntries here, which creates own
            // data properties (including `__proto__`) instead of assigning.
            for (k, v) in b.entries() {
                if let Some(v) = v.as_bool() {
                    plugins.insert(k.clone(), JsValue::Bool(v));
                }
            }
        }
        if let Some(v) = e.get("includeHookEvents").and_then(|v| v.as_bool()) {
            hook = JsValue::Bool(v);
        }
    }
    put(&mut out, "builtinPluginsEnabled", JsValue::Object(plugins));
    put(&mut out, "includeHookEvents", hook);
    JsValue::Object(out)
}

// ---------------------------------------------------------------------------
// model
// ---------------------------------------------------------------------------

fn catalog() -> &'static JsValue {
    static DATA: OnceLock<JsValue> = OnceLock::new();
    DATA.get_or_init(|| {
        pilotdeck_protocol::parse_json(include_str!("catalog-data.json")).expect("catalog")
    })
}

#[derive(Clone, Copy, PartialEq)]
enum MatchType {
    Exact,
    Alias,
    CrossProvider,
    None,
}

fn catalog_provider(id: &JsString) -> Option<&'static JsValue> {
    catalog().as_object().and_then(|o| o.get_key(id))
}
fn catalog_model_in_provider(
    provider: &'static JsValue,
    id: &JsString,
) -> Option<(&'static JsValue, MatchType)> {
    let models = provider.get("models")?.as_object()?;
    if let Some(m) = models.get_key(id) {
        return Some((m, MatchType::Exact));
    }
    for (_, m) in models.entries() {
        if let Some(JsValue::Array(a)) = m.get("aliases")
            && a.iter().any(|v| v.as_string() == Some(id))
        {
            return Some((m, MatchType::Alias));
        }
    }
    None
}
fn search_all(id: &JsString) -> Option<(&'static JsValue, MatchType)> {
    for (_, p) in entries(catalog()) {
        if let Some(m) = p
            .get("models")
            .and_then(|m| m.as_object())
            .and_then(|m| m.get_key(id))
        {
            return Some((m, MatchType::CrossProvider));
        }
    }
    for (_, p) in entries(catalog()) {
        if let Some((m, _)) = catalog_model_in_provider(p, id) {
            return Some((m, MatchType::CrossProvider));
        }
    }
    None
}
fn lookup_model(provider: &JsString, id: &JsString) -> (Option<&'static JsValue>, MatchType) {
    let p = catalog_provider(provider);
    if let Some(p) = p
        && let Some((m, t)) = catalog_model_in_provider(p, id)
    {
        return (Some(m), t);
    }
    if let Some((m, _)) = search_all(id) {
        return (Some(m), MatchType::CrossProvider);
    }
    if let Some(i) = id.units().iter().position(|u| *u == 47) {
        let stripped = JsString::from_utf16(id.units()[i + 1..].to_vec());
        if let Some((m, _)) = search_all(&stripped) {
            return (Some(m), MatchType::CrossProvider);
        }
    }
    (None, MatchType::None)
}

fn protocol_capabilities(protocol: &str) -> JsValue {
    let (tool, parallel, json, cache, ctx) = match protocol {
        "anthropic" => (true, false, true, true, 200000.0),
        "google" => (true, true, true, false, 1_048_576.0),
        _ => (true, true, true, false, 128000.0),
    };
    let mut o = obj();
    put(&mut o, "supportsToolUse", JsValue::Bool(tool));
    put(&mut o, "supportsStreaming", JsValue::Bool(true));
    put(&mut o, "supportsParallelToolCalls", JsValue::Bool(parallel));
    put(&mut o, "supportsThinking", JsValue::Bool(true));
    put(&mut o, "supportsJsonSchema", JsValue::Bool(json));
    put(&mut o, "supportsSystemPrompt", JsValue::Bool(true));
    put(&mut o, "supportsPromptCache", JsValue::Bool(cache));
    put(&mut o, "maxContextTokens", num(ctx));
    put(&mut o, "maxOutputTokens", num(32768.0));
    JsValue::Object(o)
}

fn model(raw: &JsValue, providers: &[Provider], env: &Env) -> JsValue {
    let mut out_providers = obj();
    let raw_providers = field(field(raw, "model"), "providers");
    for (provider_id, raw_provider) in entries(raw_providers) {
        if !providers
            .iter()
            .any(|p| p.id.units() == provider_id.units())
        {
            continue;
        }
        let Some(p) = raw_provider.as_object() else {
            continue;
        };
        let cat = catalog_provider(provider_id);
        let raw_protocol = p.get("protocol");
        let protocol = if ["openai", "openai-responses", "anthropic", "google"]
            .iter()
            .any(|s| eq(raw_protocol.unwrap_or(&undefined()), s))
        {
            text(raw_protocol.unwrap_or(&undefined())).unwrap_or_else(|| "openai".into())
        } else {
            cat.and_then(|c| c.get("protocol"))
                .and_then(|v| v.as_string())
                .and_then(|s| s.to_utf8().ok())
                .unwrap_or_else(|| "openai".into())
        };
        let raw_url = p
            .get("url")
            .and_then(|v| v.as_string())
            .map(trim)
            .filter(|s| !s.is_empty());
        let url = raw_url
            .clone()
            .or_else(|| {
                if provider_id.equals_str("google") && protocol == "openai" {
                    Some(JsString::from(
                        "https://generativelanguage.googleapis.com/v1beta/openai",
                    ))
                } else {
                    cat.and_then(|c| c.get("defaultUrl"))
                        .and_then(|v| v.as_string())
                        .cloned()
                }
            })
            .unwrap_or_default();
        let missing = undefined();
        let raw_key = p.get("apiKey").unwrap_or(&missing);
        let catalog_env = catalog_api_key_env(provider_id, &protocol, &url);
        let (api_key, credential_source) =
            provider_credential(provider_id, raw_key, env, catalog_env.as_deref());
        let mut out = obj();
        put(&mut out, "id", JsValue::String(provider_id.clone()));
        put(&mut out, "protocol", string(&protocol));
        put(&mut out, "url", JsValue::String(url.clone()));
        put(&mut out, "apiKey", JsValue::String(api_key));
        put(&mut out, "credentialSource", string(&credential_source));
        put(
            &mut out,
            "timeoutMs",
            p.get("timeoutMs")
                .and_then(|v| v.as_number().filter(|n| n.is_finite() && *n > 0.0))
                .map(num)
                .unwrap_or_else(undefined),
        );
        let headers = match p.get("headers").and_then(|v| v.as_object()) {
            Some(h) => {
                let mut o = obj();
                for (k, v) in h.entries() {
                    // Baseline's readStringRecord builds headers by assignment,
                    // so an own `__proto__` key never becomes an own property.
                    put_key(&mut o, k.clone(), v.clone());
                }
                JsValue::Object(o)
            }
            None => JsValue::Object(obj()),
        };
        put(&mut out, "headers", headers);
        let extra = p
            .get("extraBody")
            .and_then(|v| v.as_object())
            .map(sanitize_body);
        put(&mut out, "extraBody", extra.unwrap_or_else(undefined));
        let speed = match p.get("speedMapping") {
            Some(v) if !v.is_undefined() => v.clone(),
            _ => {
                let catalog = cat.is_some();
                if catalog
                    && provider_id.equals_str("openai")
                    && (protocol == "openai" || protocol == "openai-responses")
                {
                    string("openai_service_tier")
                } else if catalog && provider_id.equals_str("anthropic") && protocol == "anthropic"
                {
                    string("anthropic_speed")
                } else {
                    undefined()
                }
            }
        };
        put(&mut out, "speedMapping", speed);
        let retry = p
            .get("retry")
            .and_then(|v| v.as_object())
            .map(|r| {
                let mut o = obj();
                for key in [
                    "requestMaxRetries",
                    "streamMaxRetries",
                    "streamIdleTimeoutMs",
                    "maxStreamingDurationMs",
                    "repeatedChunkLimit",
                    "baseDelayMs",
                    "maxDelayMs",
                    "jitter",
                ] {
                    if let Some(v) = r.get(key).filter(|v| !v.is_undefined()) {
                        put(&mut o, key, v.clone());
                    }
                }
                o
            })
            .filter(|o| !o.is_empty());
        put(
            &mut out,
            "retry",
            retry.map(JsValue::Object).unwrap_or_else(undefined),
        );
        let mut models = obj();
        if let Some(raw_models) = p.get("models").and_then(|v| v.as_object()) {
            for (model_id, raw_model) in raw_models.entries() {
                if let Some(m) = model_definition(model_id, &protocol, provider_id, raw_model) {
                    put_key(&mut models, model_id.clone(), m);
                }
            }
        }
        put(&mut out, "models", JsValue::Object(models));
        put_key(
            &mut out_providers,
            provider_id.clone(),
            JsValue::Object(out),
        );
    }
    let mut model = obj();
    put(&mut model, "providers", JsValue::Object(out_providers));
    JsValue::Object(model)
}

fn sanitize_body(body: &JsObject) -> JsValue {
    let mut out = obj();
    for (k, v) in body.entries() {
        if k.equals_str("temperature") {
            continue;
        }
        if k.equals_str("generationConfig") && v.as_object().is_some() {
            let mut g = obj();
            for (gk, gv) in v.as_object().unwrap().entries() {
                if gk.equals_str("temperature") {
                    continue;
                }
                g.insert(gk.clone(), gv.clone());
            }
            put(&mut out, "generationConfig", JsValue::Object(g));
        } else {
            // Baseline's object rest copies own enumerable keys as data
            // properties, so an own `__proto__` key is preserved here.
            out.insert(k.clone(), v.clone());
        }
    }
    JsValue::Object(out)
}

fn catalog_api_key_env(provider: &JsString, protocol: &str, url: &JsString) -> Option<String> {
    let cat = catalog_provider(provider)?;
    let env = cat.get("apiKeyEnvVar")?.as_string()?.to_utf8().ok()?;
    let cat_protocol = cat.get("protocol")?.as_string()?.to_utf8().ok()?;
    if protocol != cat_protocol && !(provider.equals_str("google") && protocol == "openai") {
        return None;
    }
    let expected = if provider.equals_str("google") && protocol == "openai" {
        "https://generativelanguage.googleapis.com/v1beta/openai".to_owned()
    } else {
        cat.get("defaultUrl")?.as_string()?.to_utf8().ok()?
    };
    if canonical_url(&expected)? != canonical_url(&String::from_utf16_lossy(url.units()))? {
        return None;
    }
    Some(env)
}
fn canonical_url(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(parsed.to_string().trim_end_matches('/').to_owned())
}
fn provider_credential(
    provider: &JsString,
    value: &JsValue,
    env: &Env,
    catalog_env: Option<&str>,
) -> (JsString, String) {
    if provider.equals_str("ollama") && value.is_undefined() {
        return ("ollama".into(), "provider_default".into());
    }
    let blank = value.as_string().is_some_and(|s| trim(s).is_empty());
    let has_value = !value.is_undefined() && !matches!(value, JsValue::Null) && !blank;
    let effective = if has_value {
        value.clone()
    } else if let Some(name) = catalog_env {
        JsValue::String(format!("${{{name}}}").into())
    } else {
        value.clone()
    };
    let source = if effective.as_string().and_then(env_reference).is_some() {
        "environment"
    } else {
        "literal"
    };
    let resolved = match effective.as_string() {
        Some(s) => match env_reference(s) {
            Some(name) => env
                .get(&String::from_utf16_lossy(name.units()))
                .map(|v| crate::metadata::js_trim(v))
                .filter(|v| !v.is_empty())
                .unwrap_or_default()
                .into(),
            None => trim(s),
        },
        None => JsString::default(),
    };
    (resolved, source.into())
}

/// `ENV_REFERENCE_PATTERN` from the baseline: only `${NAME}` where `NAME` is
/// ASCII `[A-Za-z_][A-Za-z0-9_]*` is an environment reference. `$`, Unicode
/// identifier characters, hyphens, digits-first and whitespace are literals.
fn env_reference(value: &JsString) -> Option<JsString> {
    let t = trim(value);
    let units = t.units();
    if units.len() < 4 || units[0] != 36 || units[1] != 123 || units[units.len() - 1] != 125 {
        return None;
    }
    let name = &units[2..units.len() - 1];
    let first = |c: u16| c == 95 || (65..=90).contains(&c) || (97..=122).contains(&c);
    let rest = |c: u16| first(c) || (48..=57).contains(&c);
    if !first(*name.first()?) || !name[1..].iter().copied().all(rest) {
        return None;
    }
    Some(JsString::from_utf16(name.to_vec()))
}

fn model_definition(
    model_id: &JsString,
    protocol: &str,
    provider: &JsString,
    raw: &JsValue,
) -> Option<JsValue> {
    let empty = object([]);
    let raw = if nullish(raw) { &empty } else { raw };
    let r = raw.as_object()?;
    let (cat_model, match_type) = lookup_model(provider, model_id);
    let mut out = obj();
    let thinking = r
        .get("thinking")
        .and_then(|v| thinking_settings(v, protocol));
    put(&mut out, "thinking", thinking.unwrap_or_else(undefined));
    put(&mut out, "id", JsValue::String(model_id.clone()));
    let display = r
        .get("displayName")
        .and_then(|v| v.as_string())
        .map(|s| JsValue::String(s.clone()))
        .or_else(|| {
            cat_model
                .and_then(|m| m.get("displayName"))
                .and_then(|v| v.as_string())
                .map(|s| JsValue::String(s.clone()))
        });
    put(&mut out, "displayName", display.unwrap_or_else(undefined));
    put(
        &mut out,
        "capabilities",
        capabilities(
            protocol,
            r.get("capabilities"),
            cat_model.and_then(|m| m.get("capabilities")),
            provider,
        ),
    );
    let multimodal = match (r.get("multimodal"), match_type) {
        (None, MatchType::Exact | MatchType::Alias) => cat_model
            .and_then(|m| m.get("multimodal"))
            .cloned()
            .unwrap_or_else(|| object([("input", JsValue::Array(vec![string("text")]))])),
        (None, _) => object([("input", JsValue::Array(vec![string("text")]))]),
        (Some(_), _) => multimodal(raw, match_type, cat_model),
    };
    put(&mut out, "multimodal", multimodal);
    put(
        &mut out,
        "aliases",
        r.get("aliases").cloned().unwrap_or_else(undefined),
    );
    Some(JsValue::Object(out))
}

fn thinking_settings(raw: &JsValue, protocol: &str) -> Option<JsValue> {
    if raw.is_undefined() {
        return None;
    }
    let r = raw.as_object()?;
    let state = r
        .get("state")
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_else(|| "default".into());
    let efforts_raw = match r.get("efforts") {
        Some(JsValue::Array(items)) => items.clone(),
        _ => vec![],
    };
    let known = ["low", "medium", "high", "xhigh", "max"];
    let efforts: Vec<JsValue> = known
        .iter()
        .filter(|k| efforts_raw.iter().any(|e| eq(e, k)))
        .map(|k| string(k))
        .collect();
    let allowed: &[&str] = match protocol {
        "anthropic" => &["provider", "anthropic", "server-default"],
        "google" => &["provider", "google", "server-default"],
        "openai-responses" => &["provider", "openai", "server-default"],
        _ => &[
            "provider",
            "openai",
            "thinking-type",
            "qwen-cloud",
            "qwen-local",
            "openrouter",
            "server-default",
        ],
    };
    let format = r
        .get("format")
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok())
        .filter(|s| allowed.contains(&s.as_str()))
        .unwrap_or_else(|| "provider".into());
    let mut o = obj();
    put(&mut o, "state", string(&state));
    put(&mut o, "efforts", JsValue::Array(efforts));
    put(&mut o, "format", string(&format));
    Some(JsValue::Object(o))
}

fn capabilities(
    protocol: &str,
    raw: Option<&JsValue>,
    catalog_caps: Option<&JsValue>,
    provider: &JsString,
) -> JsValue {
    let mut caps = match catalog_caps.and_then(|v| v.as_object()) {
        Some(c) => {
            let mut o = obj();
            for (k, v) in c.entries() {
                o.insert(k.clone(), v.clone());
            }
            JsValue::Object(o)
        }
        None => protocol_capabilities(protocol),
    };
    let Some(r) = raw.and_then(|v| v.as_object()) else {
        if (provider.equals_str("openai") || provider.equals_str("anthropic"))
            && let Some(o) = caps.as_object().cloned()
        {
            let mut o = o;
            put(&mut o, "supportsSpeed", JsValue::Bool(true));
            caps = JsValue::Object(o);
        }
        return caps;
    };
    let mut o = caps.as_object().unwrap().clone();
    for key in [
        "supportsToolUse",
        "supportsStreaming",
        "supportsParallelToolCalls",
        "supportsThinking",
        "supportsSpeed",
        "supportsJsonSchema",
        "supportsSystemPrompt",
        "supportsPromptCache",
    ] {
        if let Some(v) = r.get(key).and_then(|v| v.as_bool()) {
            put(&mut o, key, JsValue::Bool(v));
        }
    }
    if r.get("contextWindow").is_some_and(|v| !v.is_undefined())
        && r.get("maxContextTokens").is_none_or(|v| v.is_undefined())
        && let Some(n) = r
            .get("contextWindow")
            .and_then(|v| v.as_number())
            .filter(|n| n.is_finite() && *n > 0.0)
    {
        put(&mut o, "maxContextTokens", num(n));
    }
    for key in ["maxContextTokens", "maxOutputTokens"] {
        if let Some(n) = r
            .get(key)
            .and_then(|v| v.as_number())
            .filter(|n| n.is_finite() && *n > 0.0)
        {
            put(&mut o, key, num(n));
        }
    }
    if let Some(v) = r.get("supportsThinking").and_then(|v| v.as_bool()) {
        put(&mut o, "supportsThinkingExplicit", JsValue::Bool(v));
    }
    let explicit_speed = r.get("supportsSpeed").and_then(|v| v.as_bool());
    if explicit_speed.is_none()
        && (provider.equals_str("openai") || provider.equals_str("anthropic"))
    {
        put(&mut o, "supportsSpeed", JsValue::Bool(true));
    }
    JsValue::Object(o)
}

fn multimodal(
    raw_model: &JsValue,
    match_type: MatchType,
    cat_model: Option<&'static JsValue>,
) -> JsValue {
    let Some(raw_model) = raw_model.as_object() else {
        return object([("input", JsValue::Array(vec![string("text")]))]);
    };
    if raw_model.get("multimodal").is_none_or(|v| v.is_undefined()) {
        return match match_type {
            MatchType::Exact | MatchType::Alias => cat_model
                .and_then(|m| m.get("multimodal"))
                .cloned()
                .unwrap_or_else(|| object([("input", JsValue::Array(vec![string("text")]))])),
            _ => object([("input", JsValue::Array(vec![string("text")]))]),
        };
    }
    let Some(mm) = raw_model.get("multimodal").and_then(|v| v.as_object()) else {
        return object([("input", JsValue::Array(vec![string("text")]))]);
    };
    let mut o = obj();
    put(
        &mut o,
        "input",
        mm.get("input")
            .cloned()
            .unwrap_or_else(|| JsValue::Array(vec![string("text")])),
    );
    for key in ["maxImagesPerRequest", "maxImageBytes"] {
        put(&mut o, key, mm.get(key).cloned().unwrap_or_else(undefined));
    }
    put(
        &mut o,
        "supportedImageMimeTypes",
        mm.get("supportedImageMimeTypes")
            .cloned()
            .unwrap_or_else(undefined),
    );
    for key in ["maxPdfPages", "maxPdfBytes", "maxAudioSeconds"] {
        put(&mut o, key, mm.get(key).cloned().unwrap_or_else(undefined));
    }
    put(
        &mut o,
        "imageDetail",
        mm.get("imageDetail").cloned().unwrap_or_else(undefined),
    );
    JsValue::Object(o)
}

// ---------------------------------------------------------------------------
// memory / gateway / adapters / proxy / telemetry
// ---------------------------------------------------------------------------

fn memory(raw: &JsValue, home: &Path) -> Option<JsValue> {
    let m = field(raw, "memory");
    let m = m.as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "enabled",
        JsValue::Bool(bool_field(m.get("enabled").unwrap_or(&undefined()), true)),
    );
    put(
        &mut o,
        "provider",
        m.get("provider")
            .cloned()
            .filter(|v| v.as_string().is_some())
            .unwrap_or_else(|| string("edgeclaw")),
    );
    let root = m
        .get("rootDir")
        .and_then(|v| v.as_string())
        .map(|s| JsValue::String(s.clone()))
        .unwrap_or_else(|| {
            JsValue::String(
                crate::metadata::resolve(home, "memory")
                    .to_string_lossy()
                    .into_owned()
                    .into(),
            )
        });
    put(&mut o, "rootDir", root);
    let capture = m
        .get("captureStrategy")
        .and_then(|v| v.as_string())
        .filter(|s| s.equals_str("last_turn") || s.equals_str("full_session"))
        .cloned()
        .map(JsValue::String)
        .unwrap_or_else(|| string("last_turn"));
    put(&mut o, "captureStrategy", capture);
    put(
        &mut o,
        "includeAssistant",
        JsValue::Bool(bool_field(
            m.get("includeAssistant").unwrap_or(&undefined()),
            true,
        )),
    );
    put(
        &mut o,
        "maxMessageChars",
        m.get("maxMessageChars")
            .and_then(|v| v.as_number().filter(|n| n.is_finite() && *n > 0.0))
            .map(num)
            .unwrap_or_else(undefined),
    );
    put(
        &mut o,
        "retrievalTimeoutMs",
        m.get("retrievalTimeoutMs")
            .and_then(floor_positive)
            .map(num)
            .unwrap_or_else(undefined),
    );
    let model = match m.get("model") {
        Some(v) if !nullish(v) && !v.as_string().is_some_and(|s| trim(s).is_empty()) => v.clone(),
        _ => undefined(),
    };
    put(&mut o, "model", model);
    put(
        &mut o,
        "apiType",
        m.get("apiType").cloned().unwrap_or_else(undefined),
    );
    put(
        &mut o,
        "schedule",
        memory_schedule(m).unwrap_or_else(undefined),
    );
    put(
        &mut o,
        "heartbeatBatchSize",
        m.get("heartbeatBatchSize")
            .and_then(floor_positive)
            .map(num)
            .unwrap_or_else(undefined),
    );
    Some(JsValue::Object(o))
}

fn memory_schedule(m: &JsObject) -> Option<JsValue> {
    if let Some(schedule) = m.get("schedule") {
        let s = schedule.as_object()?;
        let mut o = obj();
        if let Some(v) = s
            .get("reasoningMode")
            .and_then(|v| v.as_string())
            .filter(|s| s.equals_str("answer_first") || s.equals_str("accuracy_first"))
        {
            put(&mut o, "reasoningMode", JsValue::String(v.clone()));
        }
        for key in ["autoIndexIntervalMinutes", "autoDreamIntervalMinutes"] {
            if let Some(n) = s.get(key).and_then(floor_non_negative) {
                put(&mut o, key, num(n));
            }
        }
        if !o.is_empty() {
            return Some(JsValue::Object(o));
        }
    }
    let mut o = obj();
    if let Some(v) = m
        .get("reasoningMode")
        .and_then(|v| v.as_string())
        .filter(|s| s.equals_str("answer_first") || s.equals_str("accuracy_first"))
    {
        put(&mut o, "reasoningMode", JsValue::String(v.clone()));
    }
    for key in ["autoIndexIntervalMinutes", "autoDreamIntervalMinutes"] {
        if let Some(n) = m.get(key).and_then(|v| v.as_number()).filter(|n| *n >= 0.0) {
            put(&mut o, key, num(n));
        }
    }
    (!o.is_empty()).then_some(JsValue::Object(o))
}

fn gateway(raw: &JsValue) -> Option<JsValue> {
    let g = field(raw, "gateway").as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "port",
        num(finite(g.get("port").unwrap_or(&undefined())).unwrap_or(18789.0)),
    );
    put(&mut o, "bindAddress", string("127.0.0.1"));
    put(
        &mut o,
        "idleSessionTimeoutMinutes",
        num(finite(g.get("idleSessionTimeoutMinutes").unwrap_or(&undefined())).unwrap_or(30.0)),
    );
    put(
        &mut o,
        "idleSweepIntervalSeconds",
        num(finite(g.get("idleSweepIntervalSeconds").unwrap_or(&undefined())).unwrap_or(60.0)),
    );
    put(
        &mut o,
        "memoryDiagnostics",
        JsValue::Bool(bool_field(
            g.get("memoryDiagnostics").unwrap_or(&undefined()),
            false,
        )),
    );
    put(
        &mut o,
        "staticAssetsPath",
        g.get("staticAssetsPath")
            .and_then(|v| v.as_string())
            .map(|s| JsValue::String(s.clone()))
            .unwrap_or_else(undefined),
    );
    let max = finite(g.get("maxPerSessionMcpInstances").unwrap_or(&undefined()))
        .unwrap_or(5.0)
        .max(1.0);
    put(&mut o, "maxPerSessionMcpInstances", num(max));
    Some(JsValue::Object(o))
}

fn adapters(raw: &JsValue) -> Option<JsValue> {
    let a = field(raw, "adapters").as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "cli",
        auto_connect(a.get("cli")).unwrap_or_else(undefined),
    );
    put(
        &mut o,
        "tui",
        auto_connect(a.get("tui")).unwrap_or_else(undefined),
    );
    put(
        &mut o,
        "feishu",
        feishu(a.get("feishu")).unwrap_or_else(undefined),
    );
    put(
        &mut o,
        "weixin",
        enabled_only(a.get("weixin")).unwrap_or_else(undefined),
    );
    put(&mut o, "qq", qq(a.get("qq")).unwrap_or_else(undefined));
    for key in [
        "telegram",
        "discord",
        "slack",
        "matrix",
        "mattermost",
        "signal",
        "whatsapp",
        "bluebubbles",
        "dingtalk",
        "wecom",
        "wecomCallback",
        "email",
        "sms",
        "homeassistant",
        "apiServer",
        "webhook",
    ] {
        if let Some(v) = platform(a.get(key)) {
            put(&mut o, key, v);
        }
    }
    Some(JsValue::Object(o))
}

fn auto_connect(raw: Option<&JsValue>) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "autoConnectServer",
        JsValue::Bool(bool_field(
            r.get("autoConnectServer").unwrap_or(&undefined()),
            true,
        )),
    );
    Some(JsValue::Object(o))
}
fn enabled_only(raw: Option<&JsValue>) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "enabled",
        JsValue::Bool(bool_field(r.get("enabled").unwrap_or(&undefined()), false)),
    );
    Some(JsValue::Object(o))
}
fn platform(raw: Option<&JsValue>) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "enabled",
        JsValue::Bool(bool_field(r.get("enabled").unwrap_or(&undefined()), false)),
    );
    for key in ["token", "apiKey", "webhookUrl"] {
        put(
            &mut o,
            key,
            r.get(key)
                .and_then(|v| v.as_string())
                .map(|s| JsValue::String(s.clone()))
                .unwrap_or_else(undefined),
        );
    }
    put(
        &mut o,
        "extra",
        r.get("extra")
            .and_then(|v| v.as_object())
            .map(|o| JsValue::Object(o.clone()))
            .unwrap_or_else(undefined),
    );
    Some(JsValue::Object(o))
}
fn feishu(raw: Option<&JsValue>) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "enabled",
        JsValue::Bool(bool_field(r.get("enabled").unwrap_or(&undefined()), false)),
    );
    for key in ["appId", "appSecret", "encryptKey", "verifyToken"] {
        put(
            &mut o,
            key,
            r.get(key)
                .and_then(|v| v.as_string())
                .map(|s| JsValue::String(s.clone()))
                .unwrap_or_else(undefined),
        );
    }
    let label = r
        .get("defaultSessionLabel")
        .and_then(|v| v.as_string())
        .map(|s| JsValue::String(s.clone()))
        .unwrap_or_else(|| string("general"));
    put(&mut o, "defaultSessionLabel", label);
    let mode = r
        .get("connectionMode")
        .and_then(|v| v.as_string())
        .filter(|s| s.equals_str("stream") || s.equals_str("webhook"))
        .map(|s| JsValue::String(s.clone()))
        .unwrap_or_else(undefined);
    put(&mut o, "connectionMode", mode);
    let domain = r
        .get("domainName")
        .and_then(|v| v.as_string())
        .filter(|s| s.equals_str("feishu") || s.equals_str("lark"))
        .map(|s| JsValue::String(s.clone()))
        .unwrap_or_else(undefined);
    put(&mut o, "domainName", domain);
    let permission = r
        .get("permissionMode")
        .and_then(|v| v.as_string())
        .filter(|s| s.equals_str("default") || s.equals_str("bypassPermissions"))
        .map(|s| JsValue::String(s.clone()))
        .unwrap_or_else(undefined);
    put(&mut o, "permissionMode", permission);
    Some(JsValue::Object(o))
}
fn qq(raw: Option<&JsValue>) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "enabled",
        JsValue::Bool(bool_field(r.get("enabled").unwrap_or(&undefined()), false)),
    );
    for key in ["appId", "clientSecret"] {
        put(
            &mut o,
            key,
            r.get(key)
                .and_then(|v| v.as_string())
                .map(|s| JsValue::String(s.clone()))
                .unwrap_or_else(undefined),
        );
    }
    for key in ["allowGroups", "triggerPrefixes"] {
        let value = match r.get(key) {
            Some(JsValue::Array(items)) => Some(JsValue::Array(
                items
                    .iter()
                    .filter(|v| v.as_string().is_some())
                    .cloned()
                    .collect(),
            )),
            _ => None,
        };
        put(&mut o, key, value.unwrap_or_else(undefined));
    }
    put(
        &mut o,
        "maxMessageLength",
        r.get("maxMessageLength")
            .and_then(|v| v.as_number().filter(|n| n.is_finite()))
            .map(num)
            .unwrap_or_else(undefined),
    );
    Some(JsValue::Object(o))
}

fn proxy(raw: &JsValue) -> Option<JsValue> {
    let legacy = field(field(field(raw, "webui"), "runtime"), "httpsProxy");
    let mut value = field(raw, "proxy").clone();
    if value.is_undefined()
        && field(raw, "webui").as_object().is_some()
        && field(field(raw, "webui"), "runtime").as_object().is_some()
        && legacy.as_string().is_some_and(|s| !s.is_empty())
    {
        value = object([("url", legacy.clone())]);
    }
    if value.as_string().is_some() {
        return Some(object([("url", value)]));
    }
    let o = value.as_object()?;
    let url = o.get("url")?.as_string()?.clone();
    if url.is_empty() {
        return None;
    }
    let mut out = obj();
    put(&mut out, "url", JsValue::String(url));
    if let Some(no_proxy) = o
        .get("noProxy")
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
    {
        put(&mut out, "noProxy", JsValue::String(no_proxy.clone()));
    }
    Some(JsValue::Object(out))
}

fn telemetry(raw: &JsValue) -> JsValue {
    let enabled = field(raw, "telemetry")
        .as_object()
        .is_some_and(|o| o.get("enabled").and_then(|v| v.as_bool()) == Some(true));
    object([("enabled", JsValue::Bool(enabled))])
}

// ---------------------------------------------------------------------------
// alwaysOn / cron / tools / runPolicy
// ---------------------------------------------------------------------------

fn always_on(raw: &JsValue, cwd: &Path) -> Option<JsValue> {
    let a = field(raw, "alwaysOn").as_object()?;
    let mut trigger = obj();
    put(&mut trigger, "tickIntervalMinutes", num(5.0));
    put(&mut trigger, "cooldownMinutes", num(60.0));
    put(&mut trigger, "dailyBudget", num(4.0));
    put(&mut trigger, "heartbeatStaleSeconds", num(90.0));
    put(&mut trigger, "recentUserMsgMinutes", num(5.0));
    put(&mut trigger, "preferChannel", string("web"));
    if let Some(t) = a.get("trigger").and_then(|v| v.as_object()) {
        for (key, fallback, positive, integer) in [
            ("tickIntervalMinutes", 5.0, true, false),
            ("cooldownMinutes", 60.0, false, false),
            ("dailyBudget", 4.0, false, true),
            ("heartbeatStaleSeconds", 90.0, true, false),
            ("recentUserMsgMinutes", 5.0, false, false),
        ] {
            if let Some(v) = t.get(key).filter(|v| !v.is_undefined()) {
                if number(v, positive, integer) {
                    put(&mut trigger, key, v.clone());
                } else {
                    put(&mut trigger, key, num(fallback));
                }
            }
        }
        if let Some(v) = t
            .get("preferChannel")
            .and_then(|v| v.as_string())
            .filter(|s| !s.is_empty())
        {
            put(&mut trigger, "preferChannel", JsValue::String(v.clone()));
        }
    }
    let mut out = obj();
    put(&mut out, "trigger", JsValue::Object(trigger));
    let mut projects = obj();
    if let Some(p) = a.get("projects").and_then(|v| v.as_object()) {
        for (root, value) in p.entries() {
            if trim(root).is_empty() {
                continue;
            }
            let Some(v) = value.as_object() else {
                continue;
            };
            let key = resolve_js_path(cwd, root);
            put_key(
                &mut projects,
                key,
                object([(
                    "enabled",
                    JsValue::Bool(bool_field(v.get("enabled").unwrap_or(&undefined()), false)),
                )]),
            );
        }
    }
    put(&mut out, "projects", JsValue::Object(projects));
    if let Some(lang) = a
        .get("language")
        .and_then(|v| v.as_string())
        .filter(|s| s.equals_str("en") || s.equals_str("zh-CN"))
    {
        put(&mut out, "language", JsValue::String(lang.clone()));
    }
    Some(JsValue::Object(out))
}

/// `path.resolve(cwd, value)` for a JavaScript string. Project keys may hold
/// lone UTF-16 surrogates, so the POSIX normalization is done on code units
/// instead of converting through UTF-8.
fn resolve_js_path(cwd: &Path, value: &JsString) -> JsString {
    let absolute = value.units().first() == Some(&47) || cwd.is_absolute();
    let mut input = if value.units().first() == Some(&47) {
        value.units().to_vec()
    } else {
        let mut input: Vec<u16> = cwd.to_string_lossy().encode_utf16().collect();
        input.push(47);
        input.extend_from_slice(value.units());
        input
    };
    if input.first() != Some(&47) {
        input.insert(0, 47);
    }
    let mut stack: Vec<Vec<u16>> = Vec::new();
    for segment in input.split(|unit| *unit == 47) {
        if segment.is_empty() || segment == [46] {
            continue;
        }
        if segment == [46, 46] {
            stack.pop();
            continue;
        }
        stack.push(segment.to_vec());
    }
    let mut units = Vec::new();
    if absolute {
        units.push(47);
    }
    units.extend(stack.join(&47u16));
    JsString::from_utf16(units)
}

fn cron(raw: &JsValue) -> Option<JsValue> {
    let c = field(raw, "cron").as_object()?;
    let mut o = obj();
    put(
        &mut o,
        "enabled",
        JsValue::Bool(bool_field(c.get("enabled").unwrap_or(&undefined()), true)),
    );
    let timezone = match c.get("timezone") {
        Some(v) if v.as_string().is_some_and(|s| !trim(s).is_empty()) => {
            trim(v.as_string().unwrap())
                .to_utf8()
                .unwrap_or_else(|_| "UTC".into())
        }
        _ => "UTC".into(),
    };
    let timezone = if valid_timezone(&timezone) {
        timezone
    } else {
        "UTC".into()
    };
    put(&mut o, "timezone", string(&timezone));
    for (key, fallback) in [("maxConcurrentRuns", 1.0), ("runTimeoutMinutes", 60.0)] {
        let value = c
            .get(key)
            .filter(|v| number(v, true, true))
            .and_then(|v| v.as_number())
            .unwrap_or(fallback);
        put(&mut o, key, num(value));
    }
    Some(JsValue::Object(o))
}

fn tools(raw: &JsValue) -> Option<JsValue> {
    let t = field(raw, "tools").as_object()?;
    let web = t.get("webSearch")?;
    let w = web.as_object()?;
    if w.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
        return Some(object([(
            "webSearch",
            object([("enabled", JsValue::Bool(false))]),
        )]));
    }
    let mut result = obj();
    if let Some(v) = w.get("enabled").and_then(|v| v.as_bool()) {
        put(&mut result, "enabled", JsValue::Bool(v));
    }
    if let Some(v) = w.get("provider").and_then(|v| v.as_string()) {
        put(&mut result, "provider", JsValue::String(v.clone()));
    }
    if let Some(v) = w.get("searchEngine").and_then(|v| v.as_string()) {
        put(&mut result, "searchEngine", JsValue::String(v.clone()));
    }
    if let Some(v) = w
        .get("apiKey")
        .and_then(|v| v.as_string())
        .map(trim)
        .filter(|s| !s.is_empty())
    {
        put(&mut result, "apiKey", JsValue::String(v));
    }
    if let Some(v) = w
        .get("endpoint")
        .and_then(|v| v.as_string())
        .map(trim)
        .filter(|s| !s.is_empty())
    {
        put(&mut result, "endpoint", JsValue::String(v));
    }
    if let Some(custom) = w.get("customProvider").and_then(|v| v.as_object()) {
        let mut c = obj();
        for key in ["auth", "method"] {
            if let Some(v) = custom.get(key).and_then(|v| v.as_string()) {
                put(&mut c, key, JsValue::String(v.clone()));
            }
        }
        for key in [
            "name",
            "queryParam",
            "apiKeyParam",
            "resultsPath",
            "titleField",
            "urlField",
            "snippetField",
            "sourceField",
            "publishedAtField",
        ] {
            if let Some(v) = custom
                .get(key)
                .and_then(|v| v.as_string())
                .map(trim)
                .filter(|s| !s.is_empty())
            {
                put(&mut c, key, JsValue::String(v));
            }
        }
        if !c.is_empty() {
            put(&mut result, "customProvider", JsValue::Object(c));
        }
    }
    Some(object([("webSearch", JsValue::Object(result))]))
}

fn run_policy(raw: &JsValue) -> Option<JsValue> {
    let r = field(raw, "runPolicy").as_object()?;
    let mut guard = obj();
    put(&mut guard, "enabled", JsValue::Bool(false));
    put(&mut guard, "modelFailureLimit", num(0.0));
    put(&mut guard, "toolFailureLimits", JsValue::Object(obj()));
    put(&mut guard, "toolLabels", JsValue::Object(obj()));
    if let Some(g) = r.get("failureGuard").and_then(|v| v.as_object()) {
        put(
            &mut guard,
            "enabled",
            JsValue::Bool(bool_field(g.get("enabled").unwrap_or(&undefined()), false)),
        );
        if let Some(n) = g
            .get("modelFailureLimit")
            .and_then(|v| v.as_number())
            .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
        {
            put(&mut guard, "modelFailureLimit", num(n));
        }
        if let Some(limits) = g.get("toolFailureLimits").and_then(|v| v.as_object()) {
            let re = tool_key_regex();
            let mut out = obj();
            for (k, v) in limits.entries() {
                let key = String::from_utf16_lossy(k.units());
                if re.is_match(&key)
                    && let Some(n) = v
                        .as_number()
                        .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
                {
                    put_key(&mut out, k.clone(), num(n));
                }
            }
            put(&mut guard, "toolFailureLimits", JsValue::Object(out));
        }
        if let Some(labels) = g.get("toolLabels").and_then(|v| v.as_object()) {
            let re = tool_key_regex();
            let mut out = obj();
            for (k, v) in labels.entries() {
                let key = String::from_utf16_lossy(k.units());
                if re.is_match(&key)
                    && let Some(s) = v.as_string().map(trim).filter(|s| !s.is_empty())
                {
                    put_key(&mut out, k.clone(), JsValue::String(s));
                }
            }
            put(&mut guard, "toolLabels", JsValue::Object(out));
        }
    }
    Some(object([("failureGuard", JsValue::Object(guard))]))
}

// ---------------------------------------------------------------------------
// router
// ---------------------------------------------------------------------------

const DEFAULT_TRIGGER_TIERS: &[&str] = &["complex"];
const DEFAULT_ALLOWED_TOOLS: &[&str] = &["agent", "read_file", "grep", "glob", "read_skill"];
const DEFAULT_TIER_DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "simple",
        "Simple greetings, confirmations, single-step Q&A, trivial file writes, remembering rules",
    ),
    (
        "medium",
        "Single tool call, short text generation, 1-2 file read/write, code generation",
    ),
    (
        "complex",
        "Needs sub-agent orchestration: parallel workstreams, delegation to specialized agents",
    ),
    (
        "reasoning",
        "Deep single-agent work: multi-file operations, data analysis, multi-step workflows, web research, structured reports from many sources",
    ),
];
const DEFAULT_TIER_RULES: &[&str] = &[
    "complex is ONLY for tasks that need sub-agent orchestration or parallel delegation — do NOT use it for single-agent multi-step work",
    "Multi-file operations, data analysis, and multi-step workflows without orchestration should be reasoning",
    "Simple file creation (1-2 files) or single code generation is medium",
    "Trivial greetings, confirmations, remembering rules, or reading one file and answering a short question is simple",
];

fn ref_valid(v: &JsValue, providers: &[Provider]) -> Option<(JsString, JsString)> {
    let s = v.as_string()?;
    let i = s.units().iter().position(|u| *u == 47)?;
    if i == 0 || i + 1 >= s.units().len() {
        return None;
    }
    let p = JsString::from_utf16(s.units()[..i].to_vec());
    let m = JsString::from_utf16(s.units()[i + 1..].to_vec());
    let provider = providers.iter().find(|x| x.id == p)?;
    provider.models.iter().any(|x| x.id == m).then_some((p, m))
}
fn reference(v: &JsValue, providers: &[Provider]) -> Option<JsValue> {
    let (p, m) = ref_valid(v, providers)?;
    let id = v.as_string()?.clone();
    let mut o = obj();
    put(&mut o, "id", JsValue::String(id));
    put(&mut o, "provider", JsValue::String(p));
    put(&mut o, "model", JsValue::String(m));
    Some(JsValue::Object(o))
}
fn enabled(v: Option<&JsValue>, fallback: bool) -> bool {
    v.and_then(|v| v.as_bool()).unwrap_or(fallback)
}
fn nonneg_int(v: &JsValue) -> Option<f64> {
    v.as_number()
        .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
}

fn selection_id(selection: &(JsString, JsString)) -> JsString {
    let mut units = selection.0.units().to_vec();
    units.push(47);
    units.extend_from_slice(selection.1.units());
    JsString::from_utf16(units)
}
fn selection_reference(selection: &(JsString, JsString)) -> JsValue {
    let mut o = obj();
    put(&mut o, "id", JsValue::String(selection_id(selection)));
    put(&mut o, "provider", JsValue::String(selection.0.clone()));
    put(&mut o, "model", JsValue::String(selection.1.clone()));
    JsValue::Object(o)
}
fn router(
    raw: &JsValue,
    providers: &[Provider],
    selection: &(JsString, JsString),
) -> Option<JsValue> {
    let r = field(raw, "router").as_object()?;
    if !enabled(r.get("enabled"), true) {
        return Some(object([("enabled", JsValue::Bool(false))]));
    }
    let mut out = obj();
    put(&mut out, "enabled", JsValue::Bool(true));
    if let Some(default_ref) = r
        .get("scenarios")
        .and_then(|v| v.as_object())
        .and_then(|s| s.get("default"))
        .and_then(|v| reference(v, providers))
    {
        // loadPilotConfig rewrites a disagreeing scenarios.default to agent.model
        // and emits CONFIG_MODEL_CONFLICT; the diagnostic is produced by the
        // existing router validator, so only the value is corrected here.
        let corrected = match default_ref.get("id").and_then(|v| v.as_string()) {
            Some(id) if id != &selection_id(selection) => selection_reference(selection),
            _ => default_ref,
        };
        let mut scenarios = obj();
        put(&mut scenarios, "default", corrected);
        put(&mut out, "scenarios", JsValue::Object(scenarios));
    }
    put(
        &mut out,
        "fallback",
        router_fallback(r.get("fallback"), providers).unwrap_or_else(undefined),
    );
    put(
        &mut out,
        "zeroUsageRetry",
        router_zero_usage(r.get("zeroUsageRetry")),
    );
    put(
        &mut out,
        "transientRetry",
        router_transient(r.get("transientRetry")).unwrap_or_else(undefined),
    );
    let token_saver = router_token_saver(r.get("tokenSaver"), providers);
    put(
        &mut out,
        "tokenSaver",
        token_saver.clone().unwrap_or_else(undefined),
    );
    put(
        &mut out,
        "autoOrchestrate",
        router_auto_orchestrate(r.get("autoOrchestrate"), &token_saver).unwrap_or_else(undefined),
    );
    put(
        &mut out,
        "stats",
        router_stats(r.get("stats"), providers).unwrap_or_else(undefined),
    );
    put(
        &mut out,
        "customRouter",
        r.get("customRouter")
            .and_then(|v| v.as_object())
            .and_then(|c| c.get("extensionId"))
            .and_then(|v| v.as_string())
            .map(|s| object([("extensionId", JsValue::String(s.clone()))]))
            .unwrap_or_else(undefined),
    );
    Some(JsValue::Object(out))
}

fn router_fallback(raw: Option<&JsValue>, providers: &[Provider]) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut out = obj();
    let mut max_fallbacks = None;
    for (key, value) in r.entries() {
        if key.equals_str("maxFallbacks") {
            if let Some(n) = nonneg_int(value) {
                max_fallbacks = Some(n);
            }
            continue;
        }
        let name = String::from_utf16_lossy(key.units());
        if !["default", "subagent", "explicit"].contains(&name.as_str()) {
            continue;
        }
        let JsValue::Array(items) = value else {
            continue;
        };
        let refs: Vec<_> = items
            .iter()
            .filter_map(|v| reference(v, providers))
            .collect();
        if !refs.is_empty() {
            put(&mut out, &name, JsValue::Array(refs));
        }
    }
    put(&mut out, "maxFallbacks", num(max_fallbacks.unwrap_or(5.0)));
    Some(JsValue::Object(out))
}

fn router_zero_usage(raw: Option<&JsValue>) -> JsValue {
    let mut out = obj();
    let Some(r) = raw.and_then(|v| v.as_object()) else {
        put(&mut out, "enabled", JsValue::Bool(true));
        put(&mut out, "maxAttempts", num(2.0));
        return JsValue::Object(out);
    };
    put(
        &mut out,
        "enabled",
        JsValue::Bool(enabled(r.get("enabled"), true)),
    );
    put(
        &mut out,
        "maxAttempts",
        num(r.get("maxAttempts").and_then(nonneg_int).unwrap_or(2.0)),
    );
    JsValue::Object(out)
}

fn router_transient(raw: Option<&JsValue>) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut out = obj();
    put(
        &mut out,
        "enabled",
        JsValue::Bool(enabled(r.get("enabled"), true)),
    );
    put(
        &mut out,
        "maxAttempts",
        num(r.get("maxAttempts").and_then(nonneg_int).unwrap_or(2.0)),
    );
    put(
        &mut out,
        "baseDelayMs",
        num(r
            .get("baseDelayMs")
            .and_then(|v| v.as_number().filter(|n| n.is_finite() && *n >= 0.0))
            .unwrap_or(500.0)),
    );
    put(
        &mut out,
        "maxDelayMs",
        num(r
            .get("maxDelayMs")
            .and_then(|v| v.as_number().filter(|n| n.is_finite() && *n >= 0.0))
            .unwrap_or(8000.0)),
    );
    Some(JsValue::Object(out))
}

fn router_token_saver(raw: Option<&JsValue>, providers: &[Provider]) -> Option<JsValue> {
    let r = raw?.as_object()?;
    if !enabled(r.get("enabled"), true) {
        return Some(object([("enabled", JsValue::Bool(false))]));
    }
    let judge = reference(r.get("judge")?, providers)?;
    let tiers_raw = r.get("tiers")?.as_object()?;
    if tiers_raw.is_empty() {
        return None;
    }
    let mut tiers = obj();
    for (name, body) in js_entries(tiers_raw) {
        let Some(b) = body.as_object() else {
            continue;
        };
        let Some(model) = b.get("model").and_then(|v| reference(v, providers)) else {
            continue;
        };
        let mut t = obj();
        put(&mut t, "model", model);
        put(
            &mut t,
            "label",
            b.get("label")
                .and_then(|v| v.as_string())
                .map(|s| JsValue::String(s.clone()))
                .unwrap_or_else(undefined),
        );
        let description = b
            .get("description")
            .and_then(|v| v.as_string())
            .map(|s| JsValue::String(s.clone()))
            .unwrap_or_else(|| {
                let key = String::from_utf16_lossy(name.units());
                DEFAULT_TIER_DESCRIPTIONS
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| string(v))
                    .unwrap_or_else(undefined)
            });
        put(&mut t, "description", description);
        put_key(&mut tiers, name.clone(), JsValue::Object(t));
    }
    if tiers.is_empty() {
        return None;
    }
    let first = tiers
        .entries()
        .first()
        .map(|(k, _)| String::from_utf16_lossy(k.units()))
        .unwrap_or_default();
    let default_tier = r
        .get("defaultTier")
        .and_then(|v| v.as_string())
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_else(|| {
            if tiers.entries().iter().any(|(k, _)| k.equals_str("medium")) {
                "medium".into()
            } else {
                first.clone()
            }
        });
    let default_tier = if tiers
        .entries()
        .iter()
        .any(|(k, _)| k.equals_str(&default_tier))
    {
        default_tier
    } else {
        first
    };
    let mut out = obj();
    put(&mut out, "enabled", JsValue::Bool(true));
    put(&mut out, "judge", judge);
    put(&mut out, "defaultTier", string(&default_tier));
    put(&mut out, "tiers", JsValue::Object(tiers));
    let rules = match r.get("rules") {
        None => JsValue::Array(DEFAULT_TIER_RULES.iter().map(|s| string(s)).collect()),
        Some(v) => v.clone(),
    };
    put(&mut out, "rules", rules);
    put(
        &mut out,
        "subagent",
        r.get("subagent")
            .and_then(|v| v.as_object())
            .and_then(|s| s.get("policy"))
            .and_then(|v| v.as_string())
            .filter(|s| s.equals_str("skip") || s.equals_str("judge"))
            .map(|s| object([("policy", JsValue::String(s.clone()))]))
            .unwrap_or_else(undefined),
    );
    put(
        &mut out,
        "judgeTimeoutMs",
        num(r
            .get("judgeTimeoutMs")
            .and_then(|v| {
                v.as_number()
                    .filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0)
            })
            .unwrap_or(15000.0)),
    );
    let mut cache = obj();
    if let Some(c) = r.get("cacheAwareSwitching").and_then(|v| v.as_object()) {
        put(
            &mut cache,
            "enabled",
            JsValue::Bool(enabled(c.get("enabled"), true)),
        );
        put(
            &mut cache,
            "minSavingsRatio",
            num(c
                .get("minSavingsRatio")
                .and_then(|v| v.as_number().filter(|n| n.is_finite() && *n >= 0.0))
                .unwrap_or(0.0)),
        );
    } else {
        put(&mut cache, "enabled", JsValue::Bool(true));
        put(&mut cache, "minSavingsRatio", num(0.0));
    }
    put(&mut out, "cacheAwareSwitching", JsValue::Object(cache));
    Some(JsValue::Object(out))
}

fn router_auto_orchestrate(
    raw: Option<&JsValue>,
    token_saver: &Option<JsValue>,
) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut out = obj();
    put(
        &mut out,
        "enabled",
        JsValue::Bool(enabled(r.get("enabled"), true)),
    );
    let tiers = match r.get("triggerTiers") {
        Some(JsValue::Array(items)) => JsValue::Array(items.clone()),
        _ => JsValue::Array(DEFAULT_TRIGGER_TIERS.iter().map(|s| string(s)).collect()),
    };
    put(&mut out, "triggerTiers", tiers);
    let allowed = match r.get("allowedTools") {
        Some(JsValue::Array(items)) => Some(JsValue::Array(items.clone())),
        _ => None,
    };
    let blocked = match r.get("blockedTools") {
        Some(JsValue::Array(items)) => Some(JsValue::Array(items.clone())),
        _ => None,
    };
    let allowed = allowed.or_else(|| {
        if blocked.is_none() {
            Some(JsValue::Array(
                DEFAULT_ALLOWED_TOOLS.iter().map(|s| string(s)).collect(),
            ))
        } else {
            None
        }
    });
    put(&mut out, "allowedTools", allowed.unwrap_or_else(undefined));
    put(&mut out, "blockedTools", blocked.unwrap_or_else(undefined));
    put(
        &mut out,
        "slimSystemPrompt",
        JsValue::Bool(enabled(r.get("slimSystemPrompt"), true)),
    );
    put(
        &mut out,
        "skillExtensionId",
        r.get("skillExtensionId")
            .and_then(|v| v.as_string())
            .map(|s| JsValue::String(s.clone()))
            .unwrap_or_else(undefined),
    );
    put(
        &mut out,
        "orchestrationPrompt",
        r.get("orchestrationPrompt")
            .and_then(|v| v.as_string())
            .map(|s| JsValue::String(s.clone()))
            .unwrap_or_else(undefined),
    );
    put(
        &mut out,
        "subagentMaxTokens",
        r.get("subagentMaxTokens")
            .and_then(|v| {
                v.as_number()
                    .filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0)
            })
            .map(num)
            .unwrap_or_else(undefined),
    );
    let _ = token_saver;
    Some(JsValue::Object(out))
}

fn router_stats(raw: Option<&JsValue>, providers: &[Provider]) -> Option<JsValue> {
    let r = raw?.as_object()?;
    let mut out = obj();
    put(
        &mut out,
        "enabled",
        JsValue::Bool(enabled(r.get("enabled"), true)),
    );
    let pricing = r
        .get("modelPricing")
        .and_then(|v| v.as_object())
        .map(|p| {
            let mut o = obj();
            for (key, body) in p.entries() {
                let Some(b) = body.as_object() else {
                    continue;
                };
                let id = JsValue::String(key.clone());
                if !ref_valid(&id, providers).is_some() {
                    continue;
                }
                let mut entry = obj();
                for field in ["input", "output", "cacheRead"] {
                    if let Some(v) = b
                        .get(field)
                        .and_then(|v| v.as_number().filter(|n| n.is_finite() && *n >= 0.0))
                    {
                        put(&mut entry, field, num(v));
                    }
                }
                if let Some(v) = b.get("unit").and_then(|v| v.as_string()) {
                    put(&mut entry, "unit", JsValue::String(v.clone()));
                }
                put_key(&mut o, key.clone(), JsValue::Object(entry));
            }
            JsValue::Object(o)
        })
        .unwrap_or_else(undefined);
    put(&mut out, "modelPricing", pricing);
    let baseline = r
        .get("baselineModel")
        .and_then(|v| {
            if let Some(s) = v.as_string() {
                reference(&JsValue::String(s.clone()), providers)
            } else if let Some(b) = v.as_object() {
                let p = trim(b.get("provider")?.as_string()?);
                let m = trim(b.get("model")?.as_string()?);
                if p.is_empty() || m.is_empty() {
                    return None;
                }
                let mut units = p.units().to_vec();
                units.push(47);
                units.extend_from_slice(m.units());
                reference(&JsValue::String(JsString::from_utf16(units)), providers)
            } else {
                None
            }
        })
        .unwrap_or_else(undefined);
    put(&mut out, "baselineModel", baseline);
    if let Some(n) = r.get("retentionMs").and_then(|v| {
        v.as_number()
            .filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0 && *n <= 9007199254740991.0)
    }) {
        put(&mut out, "retentionMs", num(n));
    }
    Some(JsValue::Object(out))
}

// ---------------------------------------------------------------------------
// modules
// ---------------------------------------------------------------------------

fn resolve_api_key(value: &JsValue, env: &Env) -> Option<JsString> {
    let value = trim(value.as_string()?);
    let Some(name) = env_reference(&value) else {
        return Some(value);
    };
    env.get(&String::from_utf16_lossy(name.units()))
        .map(|v| crate::metadata::js_trim(v))
        .filter(|v| !v.is_empty())
        .map(Into::into)
}

fn supported_methods(slot: &str) -> JsValue {
    field(super::modules::data(), slot)
        .get("methods")
        .cloned()
        .unwrap_or_else(|| JsValue::Array(vec![]))
}
fn supported_contract(slot: &str) -> Option<String> {
    field(super::modules::data(), slot)
        .get("contract")
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok())
}

fn modules(raw: &JsValue, home: &Path, env: &Env) -> Option<JsValue> {
    if nullish(field(raw, "modules")) {
        return None;
    }
    let m = field(raw, "modules").as_object()?;
    let mut core = Vec::new();
    for slot in super::modules::CORE {
        core.push((*slot, core_module(slot, m.get(slot))));
    }
    let external_agent_loop = core[0]
        .1
        .as_ref()
        .and_then(|v| v.as_object())
        .is_some_and(|o| o.contains_key("implementationId"));
    if external_agent_loop
        && core.iter().skip(1).any(|(_, v)| {
            v.as_ref()
                .and_then(|v| v.as_object())
                .is_some_and(|o| o.contains_key("implementationId"))
        })
    {
        // Fatal diagnostics already prevent a snapshot from being returned.
    }
    let sop = sop_module(m.get("sop"), home, env);
    let mut out = obj();
    let mut knowledge = None;
    for (slot, value) in core {
        if slot == "knowledge" {
            knowledge = value;
            continue;
        }
        if let Some(v) = value {
            put(&mut out, slot, v);
        }
    }
    if let Some(v) = sop {
        put(&mut out, "sop", v);
    }
    if let Some(v) = knowledge {
        put(&mut out, "knowledge", v);
    }
    Some(JsValue::Object(out))
}

fn core_module(slot: &str, value: Option<&JsValue>) -> Option<JsValue> {
    let v = value?;
    if nullish(v) {
        return None;
    }
    let r = v.as_object()?;
    if r.get("enabled").and_then(|v| v.as_bool()) == Some(false)
        && (slot == "skills" || slot == "knowledge")
    {
        return Some(object([("enabled", JsValue::Bool(false))]));
    }
    if r.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let external = [
        "implementationId",
        "contract",
        "transport",
        "endpoint",
        "methods",
        "deployment",
    ]
    .iter()
    .any(|key| r.get(key).is_some_and(|v| !v.is_undefined()));
    if r.get("provider").is_some_and(|v| !v.is_undefined()) && external {
        return None;
    }
    if !external {
        return r
            .get("provider")
            .is_some_and(|v| eq(v, "pilotdeck"))
            .then(|| {
                object([
                    ("enabled", JsValue::Bool(true)),
                    ("provider", string("pilotdeck")),
                ])
            });
    }
    external_module(slot, r)
}

fn nonempty(v: Option<&JsValue>) -> Option<String> {
    v.and_then(|v| v.as_string())
        .map(trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.to_utf8().ok())
}
fn nonempty_js(v: Option<&JsValue>) -> Option<JsString> {
    v.and_then(|v| v.as_string())
        .map(trim)
        .filter(|s| !s.is_empty())
}
fn has_leading_slash(value: &JsString) -> bool {
    value.units().first() == Some(&47)
}
fn string_array(value: Option<&JsValue>) -> Option<Vec<JsValue>> {
    let JsValue::Array(items) = value? else {
        return None;
    };
    if items.is_empty() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let s = trim(item.as_string()?);
        if s.is_empty() {
            return None;
        }
        if !seen.insert(s.clone()) {
            return None;
        }
        out.push(JsValue::String(s));
    }
    Some(out)
}
fn string_array_allow_empty(value: Option<&JsValue>) -> Option<Vec<JsValue>> {
    let JsValue::Array(items) = value? else {
        return None;
    };
    items
        .iter()
        .all(|v| v.as_string().is_some())
        .then(|| items.clone())
}
fn string_record(value: Option<&JsValue>) -> Option<JsObject> {
    let r = value?.as_object()?;
    if !r.entries().iter().all(|(_, v)| v.as_string().is_some()) {
        return None;
    }
    Some(r.clone())
}
fn positive_integer(value: Option<&JsValue>) -> Option<f64> {
    value?
        .as_number()
        .filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0)
}
fn port(value: Option<&JsValue>) -> Option<f64> {
    value?
        .as_number()
        .filter(|n| n.is_finite() && *n >= 1.0 && *n <= 65535.0 && n.fract() == 0.0)
}
fn http_url_ok(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}
fn deployment(value: Option<&JsValue>) -> Option<JsValue> {
    let v = value?;
    if nullish(v) {
        return None;
    }
    let d = v.as_object()?;
    let mode = nonempty(d.get("mode"))?;
    if !["build", "image", "external"].contains(&mode.as_str()) {
        return None;
    }
    if mode == "external" {
        return Some(object([("mode", string("external"))]));
    }
    let port = port(d.get("port"))?;
    let health = nonempty(d.get("healthPath"));
    if mode == "image" {
        let image = nonempty(d.get("image"))?;
        let mut o = obj();
        put(&mut o, "mode", string("image"));
        put(&mut o, "image", string(&image));
        put(&mut o, "port", num(port));
        if let Some(h) = health {
            put(&mut o, "healthPath", string(&h));
        }
        return Some(JsValue::Object(o));
    }
    let context = nonempty(d.get("context"))?;
    let dockerfile = match d.get("dockerfile") {
        None => Some("Dockerfile".to_owned()),
        Some(v) => nonempty(Some(v)),
    }?;
    let mut o = obj();
    put(&mut o, "mode", string("build"));
    put(&mut o, "context", string(&context));
    put(&mut o, "dockerfile", string(&dockerfile));
    put(&mut o, "port", num(port));
    if let Some(h) = health {
        put(&mut o, "healthPath", string(&h));
    }
    Some(JsValue::Object(o))
}
fn tool_catalog(value: Option<&JsValue>) -> Option<Vec<JsValue>> {
    let JsValue::Array(items) = value? else {
        return None;
    };
    let mut out = Vec::new();
    for raw in items {
        let Some(t) = raw.as_object() else {
            continue;
        };
        let (Some(name), Some(description)) =
            (nonempty(t.get("name")), nonempty(t.get("description")))
        else {
            continue;
        };
        let Some(input) = t.get("inputSchema").and_then(|v| v.as_object()) else {
            continue;
        };
        let mut o = obj();
        put(&mut o, "name", string(&name));
        put(&mut o, "description", string(&description));
        put(&mut o, "inputSchema", JsValue::Object(input.clone()));
        if let Some(v) = t.get("kind").and_then(|v| v.as_string()) {
            put(&mut o, "kind", JsValue::String(v.clone()));
        }
        if let Some(v) = t.get("readOnly").and_then(|v| v.as_bool()) {
            put(&mut o, "readOnly", JsValue::Bool(v));
        }
        if let Some(v) = t.get("concurrencySafe").and_then(|v| v.as_bool()) {
            put(&mut o, "concurrencySafe", JsValue::Bool(v));
        }
        if let Some(v) = t.get("requiresUserInteraction").and_then(|v| v.as_bool()) {
            put(&mut o, "requiresUserInteraction", JsValue::Bool(v));
        }
        out.push(JsValue::Object(o));
    }
    Some(out)
}

fn external_module(slot: &str, r: &JsObject) -> Option<JsValue> {
    let implementation = nonempty_js(r.get("implementationId"));
    let contract = nonempty_js(r.get("contract"));
    let transport = nonempty_js(r.get("transport"));
    let endpoint = nonempty_js(r.get("endpoint"));
    let manifest = nonempty_js(r.get("manifestPath"));
    let call = nonempty_js(r.get("callPath"));
    let methods = string_array(r.get("methods"));
    let supported = supported_methods(slot);
    let default_methods = match &supported {
        JsValue::Array(a) => a.clone(),
        _ => vec![],
    };
    let methods = methods.unwrap_or(default_methods);
    if slot == "agentLoop" {
        return external_agent_loop(r, implementation, contract, transport, methods);
    }
    let endpoint_text = endpoint.as_ref().and_then(|s| s.to_utf8().ok())?;
    if !http_url_ok(&endpoint_text) {
        return None;
    }
    let manifest = manifest.unwrap_or_else(|| "/module-manifest".into());
    let call = call.unwrap_or_else(|| "/v2/module/call".into());
    if !has_leading_slash(&manifest) || !has_leading_slash(&call) {
        return None;
    }
    let implementation = implementation?;
    let contract = contract?;
    if transport.as_ref().and_then(|s| s.to_utf8().ok()).as_deref() != Some("module-http-v2") {
        return None;
    }
    let timeout = positive_integer(r.get("timeoutMs"));
    if r.get("timeoutMs").is_some_and(|v| !v.is_undefined()) && timeout.is_none() {
        return None;
    }
    if r.get("methods").is_some_and(|v| !v.is_undefined())
        && string_array(r.get("methods")).is_none()
    {
        return None;
    }
    let tools = if slot == "tools" {
        tool_catalog(r.get("catalog"))
    } else {
        None
    };
    if slot == "tools" && tools.as_ref().is_none_or(|t| t.is_empty()) {
        return None;
    }
    let deployment = deployment(r.get("deployment"));
    let mut o = obj();
    put(&mut o, "enabled", JsValue::Bool(true));
    put(&mut o, "implementationId", JsValue::String(implementation));
    put(&mut o, "contract", JsValue::String(contract));
    put(&mut o, "transport", string("module-http-v2"));
    put(&mut o, "endpoint", JsValue::String(endpoint.unwrap()));
    put(&mut o, "manifestPath", JsValue::String(manifest));
    put(&mut o, "callPath", JsValue::String(call));
    put(&mut o, "methods", JsValue::Array(methods));
    if let Some(n) = timeout {
        put(&mut o, "timeoutMs", num(n));
    }
    if let Some(t) = tools {
        put(&mut o, "tools", JsValue::Array(t));
    }
    for key in ["frontendModule", "tenantId", "actorUserId", "agentId"] {
        if let Some(v) = nonempty_js(r.get(key)) {
            put(&mut o, key, JsValue::String(v));
        }
    }
    if slot == "knowledge"
        && let Some(v) = nonempty_js(r.get("credentialEnv"))
    {
        put(&mut o, "credentialEnv", JsValue::String(v));
    }
    if let Some(d) = deployment {
        put(&mut o, "deployment", d);
    }
    Some(JsValue::Object(o))
}

fn external_agent_loop(
    r: &JsObject,
    implementation: Option<JsString>,
    contract: Option<JsString>,
    transport: Option<JsString>,
    methods: Vec<JsValue>,
) -> Option<JsValue> {
    let implementation = implementation?;
    let expected = supported_contract("agentLoop")?;
    if contract.as_ref().and_then(|s| s.to_utf8().ok()).as_deref() != Some(expected.as_str()) {
        return None;
    }
    let transport_text = transport.as_ref().and_then(|s| s.to_utf8().ok())?;
    let deployment = deployment(r.get("deployment"));
    let mut o = obj();
    put(&mut o, "enabled", JsValue::Bool(true));
    put(&mut o, "implementationId", JsValue::String(implementation));
    put(&mut o, "contract", string(&expected));
    put(&mut o, "transport", string(&transport_text));
    put(&mut o, "methods", JsValue::Array(methods));
    match transport_text.as_str() {
        "module-stdio-v2" => {
            let command = nonempty_js(r.get("command"))?;
            let args = match r.get("args") {
                None => vec![],
                Some(v) => string_array_allow_empty(Some(v))?,
            };
            let env = match r.get("env") {
                None => None,
                Some(v) => Some(string_record(Some(v))?),
            };
            put(&mut o, "command", JsValue::String(command));
            put(&mut o, "args", JsValue::Array(args));
            if let Some(e) = env {
                put(&mut o, "env", JsValue::Object(e));
            }
        }
        "module-tcp-v2" => {
            let host = nonempty_js(r.get("host"))?;
            let port = port(r.get("port"))?;
            put(&mut o, "host", JsValue::String(host));
            put(&mut o, "port", num(port));
            if let Some(n) = positive_integer(r.get("connectTimeoutMs")) {
                put(&mut o, "connectTimeoutMs", num(n));
            }
        }
        _ => return None,
    }
    if let Some(v) = nonempty_js(r.get("frontendModule")) {
        put(&mut o, "frontendModule", JsValue::String(v));
    }
    if let Some(d) = deployment {
        put(&mut o, "deployment", d);
    }
    Some(JsValue::Object(o))
}

fn sop_module(value: Option<&JsValue>, home: &Path, env: &Env) -> Option<JsValue> {
    let v = value?;
    if nullish(v) {
        return None;
    }
    let r = v.as_object()?;
    if r.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
        return None;
    }
    if r.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let binding = ["implementationId", "contract", "transport", "manifestPath"]
        .iter()
        .any(|key| r.get(key).is_some_and(|v| !v.is_undefined()));
    if r.get("provider").is_some_and(|v| !v.is_undefined()) && binding {
        return None;
    }
    let endpoint = nonempty_js(r.get("endpoint"))?;
    let definitions = nonempty_js(r.get("definitionsPath"))?;
    let default_id = nonempty_js(r.get("defaultSopId"))?;
    let endpoint_text = endpoint.to_utf8().ok()?;
    if !http_url_ok(&endpoint_text) {
        return None;
    }
    let definitions_text = definitions.to_utf8().ok()?;
    let resolved_definitions = if Path::new(&definitions_text).is_absolute() {
        PathBuf::from(&definitions_text)
    } else {
        crate::metadata::resolve(home, &definitions_text)
    };
    let state_root = if env
        .get("STAFFDECK_PROJECT_BINDING_ENABLED")
        .map(String::as_str)
        == Some("true")
    {
        crate::metadata::resolve(&resolved_definitions, "..")
            .join("state")
            .to_string_lossy()
            .into_owned()
    } else {
        home.join("sop").to_string_lossy().into_owned()
    };
    let mut o = obj();
    put(&mut o, "endpoint", JsValue::String(endpoint));
    put(
        &mut o,
        "definitionsPath",
        JsValue::String(resolved_definitions.to_string_lossy().into_owned().into()),
    );
    put(&mut o, "defaultSopId", JsValue::String(default_id));
    put(&mut o, "stateRoot", string(&state_root));
    if let Some(n) = positive_integer(r.get("timeoutMs")) {
        put(&mut o, "timeoutMs", num(n));
    }
    if let Some(mode) = r
        .get("contextMode")
        .and_then(|v| v.as_string())
        .filter(|s| s.equals_str("new_session") || s.equals_str("inherit"))
    {
        put(&mut o, "contextMode", JsValue::String(mode.clone()));
    }
    let discovery_configured = ["discoveryEndpoint", "discoveryAgentId", "discoveryApiKey"]
        .iter()
        .any(|key| r.get(key).is_some_and(|v| !v.is_undefined()));
    if discovery_configured
        && let (Some(ep), Some(agent), Some(key)) = (
            nonempty_js(r.get("discoveryEndpoint")),
            nonempty_js(r.get("discoveryAgentId")),
            nonempty_js(r.get("discoveryApiKey")),
        )
    {
        put(&mut o, "discoveryEndpoint", JsValue::String(ep));
        put(&mut o, "discoveryAgentId", JsValue::String(agent));
        let key = resolve_api_key(&JsValue::String(key), env).unwrap_or_default();
        put(&mut o, "discoveryApiKey", JsValue::String(key));
        if let Some(p) = nonempty_js(r.get("discoveryPath")) {
            put(&mut o, "discoveryPath", JsValue::String(p));
        }
        if let Some(n) = positive_integer(r.get("discoveryTimeoutMs")) {
            put(&mut o, "discoveryTimeoutMs", num(n));
        }
    }
    if let Some(d) = deployment(r.get("deployment")) {
        put(&mut o, "deployment", d);
    }
    if !binding {
        if !r.get("provider").is_some_and(|v| eq(v, "staffdeck")) {
            return None;
        }
        let mut out = obj();
        put(&mut out, "provider", string("staffdeck"));
        for (k, v) in o.entries() {
            out.insert(k.clone(), v.clone());
        }
        return Some(JsValue::Object(out));
    }
    let implementation = nonempty_js(r.get("implementationId"))?;
    if r.get("contract")
        .and_then(|v| v.as_string())
        .is_none_or(|s| !s.equals_str("sop.lifecycle/v2"))
    {
        return None;
    }
    if r.get("transport")
        .and_then(|v| v.as_string())
        .is_none_or(|s| !s.equals_str("sop-http-v2"))
    {
        return None;
    }
    let manifest = nonempty_js(r.get("manifestPath")).unwrap_or_else(|| "/healthz".into());
    if !has_leading_slash(&manifest) {
        return None;
    }
    let mut out = obj();
    for (k, v) in o.entries() {
        out.insert(k.clone(), v.clone());
    }
    put(
        &mut out,
        "implementationId",
        JsValue::String(implementation),
    );
    put(&mut out, "contract", string("sop.lifecycle/v2"));
    put(&mut out, "transport", string("sop-http-v2"));
    put(&mut out, "manifestPath", JsValue::String(manifest));
    if let Some(v) = nonempty_js(r.get("frontendModule")) {
        put(&mut out, "frontendModule", JsValue::String(v));
    }
    Some(JsValue::Object(out))
}
