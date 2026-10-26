//! EdgeClaw LLM config resolution (A6.6-edgeclaw-5).
//!
//! Ports `service.ts:112-345` (`parseModelRef`, `resolveOpenClawModelConfig`,
//! `buildLlmConfig`, `mergeIndexingSettings`),
//! `createEdgeClawMemoryProviderFromConfig.ts:78-112` (`resolveMemoryLlm`,
//! `memoryApiTypeForProtocol`) and `llm-extraction.ts:849-908,2162-2259`.
//! `service.ts:115` `normalizeText` is trim-only, not the collapse-whitespace
//! `edgeclaw::text::normalize_text`. Env/home are injected; only
//! [`build_llm_config`] and [`resolve_open_claw_model_config`] read the process.

use super::super::storage::accessors::homedir;
use crate::runtime::memory::{MemoryConfig, MemoryModelConfig, js_trim_units};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

fn trimmed(value: Option<&JsString>) -> JsString {
    match value {
        Some(value) => JsString::from_utf16(js_trim_units(value.units()).to_vec()),
        None => JsString::default(),
    }
}

fn field(object: Option<&JsObject>, key: &str) -> JsString {
    trimmed(
        object
            .and_then(|object| object.get(key))
            .and_then(JsValue::as_string),
    )
}

fn child<'a>(object: Option<&'a JsObject>, key: &str) -> Option<&'a JsObject> {
    object?.get(key)?.as_object()
}

fn first_truthy(values: impl IntoIterator<Item = JsString>) -> JsString {
    values
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn object<K: Into<JsString>>(entries: impl IntoIterator<Item = (K, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}

/// `service.ts:146-161`: split on the first `/`, trim both halves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedModelRef {
    pub provider: Option<JsString>,
    pub model: Option<JsString>,
}

pub fn parse_model_ref(value: Option<&JsString>) -> ParsedModelRef {
    let normalized = trimmed(value);
    if normalized.is_empty() {
        return ParsedModelRef::default();
    }
    let Some(slash) = normalized
        .units()
        .iter()
        .position(|unit| *unit == u16::from(b'/'))
    else {
        return ParsedModelRef {
            provider: None,
            model: Some(normalized),
        };
    };
    let units = normalized.units();
    ParsedModelRef {
        provider: Some(trimmed(Some(&JsString::from_utf16(
            units[..slash].to_vec(),
        )))),
        model: Some(trimmed(Some(&JsString::from_utf16(
            units[slash + 1..].to_vec(),
        )))),
    }
}

/// `toStringRecord` (`service.ts:135-144`): strings only, trimmed, empties dropped.
fn to_string_record(value: Option<&JsValue>) -> Option<JsObject> {
    let record = value.and_then(JsValue::as_object)?;
    let mut out = JsObject::new();
    for (key, entry) in record.own_entries() {
        let text = trimmed(entry.as_string());
        if !text.is_empty() {
            out.insert(key.clone(), JsValue::String(text));
        }
    }
    (!out.is_empty()).then_some(out)
}

// -- service.ts:112-260 (openclaw) -----------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct OpenClawModelConfig {
    pub provider: JsString,
    pub model: JsString,
    pub api_type: Option<JsString>,
    pub base_url: Option<JsString>,
    pub api_key: Option<JsString>,
    pub headers: Option<JsObject>,
}

pub fn open_claw_config_path(home: &Path) -> PathBuf {
    home.join(".openclaw").join("openclaw.json")
}

/// Outer `None` = not read; `Some(None)` = the permanent `null` TS caches.
#[derive(Default)]
pub struct OpenClawStore {
    cached: Mutex<Option<Option<OpenClawModelConfig>>>,
}

impl OpenClawStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve_in(&self, home: &Path) -> Option<OpenClawModelConfig> {
        self.resolve(|| Some(home.to_path_buf()))
    }

    /// The home lookup only runs on the first read; a later `HOME` change no
    /// longer affects the cached result (TS computes the path at module load).
    pub fn resolve(&self, home: impl FnOnce() -> Option<PathBuf>) -> Option<OpenClawModelConfig> {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(value) = cached.as_ref() {
            return value.clone();
        }
        let value =
            home().and_then(|home| load_open_claw_model_config(&open_claw_config_path(&home)));
        *cached = Some(value.clone());
        value
    }
}

static OPEN_CLAW: LazyLock<OpenClawStore> = LazyLock::new(OpenClawStore::new);

pub fn resolve_open_claw_model_config() -> Option<OpenClawModelConfig> {
    OPEN_CLAW.resolve(|| homedir().ok())
}

/// `service.ts:185-254`: read + `JSON.parse` + structural walk; any failure is `null`.
pub fn load_open_claw_model_config(path: &Path) -> Option<OpenClawModelConfig> {
    if !path.exists() {
        return None;
    }
    let parsed = parse_json(&String::from_utf8_lossy(&fs::read(path).ok()?)).ok()?;
    let providers = child(parsed.as_object(), "models")?
        .get("providers")?
        .as_object()?;
    let primary = field(
        child(child(parsed.as_object(), "agents"), "defaults")
            .and_then(|defaults| defaults.get("model"))
            .and_then(JsValue::as_object),
        "primary",
    );
    let parsed_primary = parse_model_ref(Some(&primary));
    let available: Vec<(&JsString, &JsObject)> = providers
        .own_entries()
        .into_iter()
        .filter_map(|(name, value)| value.as_object().map(|config| (name, config)))
        .collect();
    if available.is_empty() {
        return None;
    }
    let (provider, provider_config) = parsed_primary
        .provider
        .as_ref()
        .filter(|provider| !provider.is_empty())
        .and_then(|provider| {
            available
                .iter()
                .find(|(name, _)| *name == provider)
                .copied()
        })
        .unwrap_or(available[0]);
    let provider_models: Vec<&JsObject> = match provider_config.get("models") {
        Some(JsValue::Array(models)) => models.iter().filter_map(JsValue::as_object).collect(),
        _ => Vec::new(),
    };
    let selected = parsed_primary
        .model
        .as_ref()
        .filter(|model| !model.is_empty())
        .and_then(|model| {
            provider_models.iter().copied().find(|entry| {
                field(Some(entry), "id") == *model || field(Some(entry), "name") == *model
            })
        })
        .or_else(|| provider_models.first().copied());
    let model = parsed_primary
        .model
        .clone()
        .filter(|model| !model.is_empty())
        .or_else(|| {
            let id = field(selected, "id");
            (!id.is_empty()).then_some(id)
        })
        .or_else(|| {
            let name = field(selected, "name");
            (!name.is_empty()).then_some(name)
        })?;

    let mut headers = JsObject::new();
    for record in [
        provider_config
            .get("headers")
            .and_then(|value| to_string_record(Some(value))),
        selected
            .and_then(|entry| entry.get("headers"))
            .and_then(|value| to_string_record(Some(value))),
    ]
    .into_iter()
    .flatten()
    {
        for (key, value) in record.own_entries() {
            headers.insert(key.clone(), value.clone());
        }
    }
    let pick = |key: &str| {
        let from_model = field(selected, key);
        if !from_model.is_empty() {
            return Some(from_model);
        }
        let from_provider = field(Some(provider_config), key);
        (!from_provider.is_empty()).then_some(from_provider)
    };
    let api_key = field(Some(provider_config), "apiKey");
    Some(OpenClawModelConfig {
        provider: provider.clone(),
        model,
        api_type: pick("api"),
        base_url: pick("baseUrl"),
        api_key: (!api_key.is_empty()).then_some(api_key),
        headers: (!headers.is_empty()).then_some(headers),
    })
}

// -- service.ts:262-330 (buildLlmConfig) -----------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmOptions {
    pub provider: Option<JsString>,
    pub model: Option<JsString>,
    pub model_ref: Option<JsString>,
    pub api_type: Option<JsString>,
    pub base_url: Option<JsString>,
    pub api_key: Option<JsString>,
    pub headers: Option<JsObject>,
}

fn env_trim(env: &dyn Fn(&str) -> Option<String>, key: &str) -> JsString {
    env(key)
        .map(|value| trimmed(Some(&JsString::from(value))))
        .unwrap_or_default()
}

fn merge_headers(target: &mut JsObject, source: Option<&JsObject>) {
    if let Some(source) = source {
        for (key, value) in source.own_entries() {
            target.insert(key.clone(), value.clone());
        }
    }
}

pub fn build_llm_config(options: Option<&LlmOptions>) -> JsValue {
    let open_claw = resolve_open_claw_model_config();
    build_llm_config_with(options, open_claw.as_ref(), &|key| {
        std::env::var_os(key).map(|value| value.to_string_lossy().into_owned())
    })
}

/// `buildLlmConfig` (`service.ts:262-330`) with injected openclaw + env.
pub fn build_llm_config_with(
    options: Option<&LlmOptions>,
    open_claw: Option<&OpenClawModelConfig>,
    env: &dyn Fn(&str) -> Option<String>,
) -> JsValue {
    let model_ref = first_truthy([
        trimmed(options.and_then(|options| options.model_ref.as_ref())),
        env_trim(env, "EDGECLAW_MEMORY_MODEL"),
    ]);
    let parsed = parse_model_ref((!model_ref.is_empty()).then_some(&model_ref));
    let provider = first_truthy([
        trimmed(options.and_then(|options| options.provider.as_ref())),
        env_trim(env, "EDGECLAW_MEMORY_PROVIDER"),
        parsed.provider.clone().unwrap_or_default(),
        open_claw
            .map(|config| config.provider.clone())
            .unwrap_or_default(),
        JsString::from("edgeclaw_memory"),
    ]);
    let model = first_truthy([
        trimmed(options.and_then(|options| options.model.as_ref())),
        parsed.model.clone().unwrap_or_default(),
        open_claw
            .map(|config| config.model.clone())
            .unwrap_or_default(),
        env_trim(env, "OPENAI_MODEL"),
    ]);
    let using_open_claw =
        open_claw.is_some_and(|config| provider == config.provider && model == config.model);
    let from_open_claw = |value: Option<&JsString>| {
        if using_open_claw {
            trimmed(value)
        } else {
            JsString::default()
        }
    };
    let base_url = first_truthy([
        trimmed(options.and_then(|options| options.base_url.as_ref())),
        env_trim(env, "EDGECLAW_MEMORY_BASE_URL"),
        from_open_claw(open_claw.and_then(|config| config.base_url.as_ref())),
        env_trim(env, "OPENAI_BASE_URL"),
    ]);
    let api_key = first_truthy([
        trimmed(options.and_then(|options| options.api_key.as_ref())),
        env_trim(env, "EDGECLAW_MEMORY_API_KEY"),
        from_open_claw(open_claw.and_then(|config| config.api_key.as_ref())),
        env_trim(env, "OPENAI_API_KEY"),
    ]);
    let api_type = first_truthy([
        trimmed(options.and_then(|options| options.api_type.as_ref())),
        env_trim(env, "EDGECLAW_MEMORY_API_TYPE"),
        from_open_claw(open_claw.and_then(|config| config.api_type.as_ref())),
        JsString::from("openai-responses"),
    ]);

    let mut headers = JsObject::new();
    if using_open_claw {
        merge_headers(
            &mut headers,
            open_claw.and_then(|config| config.headers.as_ref()),
        );
    }
    merge_headers(
        &mut headers,
        options.and_then(|options| options.headers.as_ref()),
    );
    let headers = (!headers.is_empty()).then_some(headers);
    let primary = if model.is_empty() {
        JsString::default()
    } else {
        let mut units = provider.units().to_vec();
        units.push(u16::from(b'/'));
        units.extend(model.units());
        JsString::from_utf16(units)
    };

    let mut provider_entries = Vec::new();
    if !api_key.is_empty() {
        provider_entries.push(("apiKey", JsValue::String(api_key)));
    }
    if !base_url.is_empty() {
        provider_entries.push(("baseUrl", JsValue::String(base_url.clone())));
    }
    provider_entries.push(("api", JsValue::String(api_type.clone())));
    let models = if model.is_empty() {
        JsValue::Array(Vec::new())
    } else {
        let mut entry = object([
            ("id", JsValue::String(model)),
            ("api", JsValue::String(api_type)),
        ]);
        if !base_url.is_empty() {
            entry.insert("baseUrl".into(), JsValue::String(base_url));
        }
        if let Some(headers) = &headers {
            entry.insert("headers".into(), JsValue::Object(headers.clone()));
        }
        JsValue::Array(vec![JsValue::Object(entry)])
    };
    provider_entries.push(("models", models));
    if let Some(headers) = headers {
        provider_entries.push(("headers", JsValue::Object(headers)));
    }
    let providers = object([(provider, JsValue::Object(object(provider_entries)))]);
    let models = object([("providers", JsValue::Object(providers))]);
    let model = object([("primary", JsValue::String(primary))]);
    let agents = object([(
        "defaults",
        JsValue::Object(object([("model", JsValue::Object(model))])),
    )]);
    JsValue::Object(object([
        ("agents", JsValue::Object(agents)),
        ("models", JsValue::Object(models)),
    ]))
}

// -- factory (`createEdgeClawMemoryProviderFromConfig.ts:78-112`) ----------

pub fn resolve_memory_llm(
    config: &MemoryConfig,
    model_config: Option<&MemoryModelConfig>,
    agent_model: Option<&str>,
) -> Option<LlmOptions> {
    let model_ref = config
        .model
        .clone()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            agent_model
                .filter(|value| !value.is_empty())
                .map(JsString::from)
        })?;
    let slash = model_ref
        .units()
        .iter()
        .position(|unit| *unit == u16::from(b'/'))?;
    let provider_id = JsString::from_utf16(model_ref.units()[..slash].to_vec());
    let provider_entry = provider_id
        .to_utf8()
        .ok()
        .and_then(|key| model_config.and_then(|model| model.providers.get(&key)));
    Some(LlmOptions {
        api_type: config.api_type.clone().or_else(|| {
            memory_api_type_for_protocol(provider_entry.and_then(|entry| entry.protocol.as_ref()))
        }),
        provider: Some(provider_id),
        model: Some(JsString::from_utf16(
            model_ref.units()[slash + 1..].to_vec(),
        )),
        base_url: provider_entry.and_then(|entry| entry.url.clone()),
        api_key: provider_entry.and_then(|entry| entry.api_key.clone()),
        ..LlmOptions::default()
    })
}

pub fn memory_api_type_for_protocol(protocol: Option<&JsString>) -> Option<JsString> {
    let protocol = protocol?;
    match protocol.to_utf8().ok()?.as_str() {
        "anthropic" | "google" | "openai-responses" => Some(protocol.clone()),
        "openai" => Some(JsString::from("openai-completions")),
        _ => None,
    }
}

// -- extractor (`llm-extraction.ts:826-908,2162-2259`) ---------------------

/// The extractor's `isRecord` (`llm-extraction.ts:826-828`) accepts arrays.
#[derive(Clone, Copy)]
enum JsRecord<'a> {
    Object(&'a JsObject),
    Array(&'a [JsValue]),
}

impl<'a> JsRecord<'a> {
    fn of(value: &'a JsValue) -> Option<Self> {
        match value {
            JsValue::Object(object) => Some(Self::Object(object)),
            JsValue::Array(items) => Some(Self::Array(items)),
            _ => None,
        }
    }

    fn get(self, key: &JsString) -> Option<&'a JsValue> {
        match self {
            Self::Object(object) => object.get_key(key),
            Self::Array(items) => items.get(key.array_index()? as usize),
        }
    }

    fn get_str(self, key: &str) -> Option<&'a JsValue> {
        self.get(&JsString::from(key))
    }

    fn entries(self) -> Vec<(JsString, &'a JsValue)> {
        match self {
            Self::Object(object) => object
                .own_entries()
                .into_iter()
                .map(|(key, value)| (key.clone(), value))
                .collect(),
            Self::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, value)| (JsString::from(index.to_string()), value))
                .collect(),
        }
    }
}

/// `llm-extraction.ts:849-856`: trimmed non-empty string values, stored untrimmed.
pub fn sanitize_headers(headers: Option<&JsValue>) -> Option<JsObject> {
    let record = headers.and_then(JsRecord::of)?;
    let mut out = JsObject::new();
    for (key, value) in record.entries() {
        if let JsValue::String(text) = value
            && !trimmed(Some(text)).is_empty()
        {
            out.insert(key, JsValue::String(text.clone()));
        }
    }
    (!out.is_empty()).then_some(out)
}

fn primary_value(record: Option<JsRecord<'_>>) -> Option<JsString> {
    let value = trimmed(
        record?
            .get_str("model")?
            .as_object()?
            .get("primary")?
            .as_string(),
    );
    (!value.is_empty()).then_some(value)
}

pub fn resolve_agent_primary_model(
    config: &JsValue,
    agent_id: Option<&JsString>,
) -> Option<JsString> {
    let agents = config.get("agents").and_then(JsRecord::of);
    if let Some(agent_id) = agent_id.filter(|agent_id| !agent_id.is_empty())
        && let Some(agent) = agents
            .and_then(|agents| agents.get(agent_id))
            .and_then(JsRecord::of)
        && let Some(primary) = primary_value(Some(agent))
    {
        return Some(primary);
    }
    primary_value(agents.and_then(|agents| agents.get_str("defaults").and_then(JsRecord::of)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigModelRef {
    pub provider: JsString,
    pub model: JsString,
}

/// The extractor's `parseModelRef` (`llm-extraction.ts:858-887`).
pub fn parse_config_model_ref(
    model_ref: Option<&JsString>,
    config: &JsValue,
) -> Option<ConfigModelRef> {
    if let Some(raw) = model_ref
        && let Some(slash) = raw.units().iter().position(|unit| *unit == u16::from(b'/'))
    {
        let provider = trimmed(Some(&JsString::from_utf16(raw.units()[..slash].to_vec())));
        let model = trimmed(Some(&JsString::from_utf16(
            raw.units()[slash + 1..].to_vec(),
        )));
        if !provider.is_empty() && !model.is_empty() {
            return Some(ConfigModelRef { provider, model });
        }
    }
    let providers = config
        .get("models")
        .and_then(JsRecord::of)?
        .get_str("providers")
        .and_then(JsRecord::of)?;
    if let Some(raw) = model_ref {
        let model = trimmed(Some(raw));
        let entries = providers.entries();
        if !model.is_empty() && entries.len() == 1 {
            return Some(ConfigModelRef {
                provider: entries[0].0.clone(),
                model,
            });
        }
    }
    for (provider, provider_config) in providers.entries() {
        let Some(provider_config) = JsRecord::of(provider_config) else {
            continue;
        };
        let Some(JsValue::Array(models)) = provider_config.get_str("models") else {
            continue;
        };
        for entry in models {
            let Some(id) = JsRecord::of(entry)
                .and_then(|entry| entry.get_str("id"))
                .and_then(JsValue::as_string)
            else {
                continue;
            };
            let model = trimmed(Some(id));
            if !model.is_empty() {
                return Some(ConfigModelRef { provider, model });
            }
        }
    }
    None
}

/// `/^[A-Z0-9_]+$/` (`llm-extraction.ts:2162-2164`).
pub fn looks_like_env_var_name(value: &JsString) -> bool {
    !value.is_empty()
        && value.units().iter().all(|unit| {
            *unit == u16::from(b'_')
                || (u16::from(b'0')..=u16::from(b'9')).contains(unit)
                || (u16::from(b'A')..=u16::from(b'Z')).contains(unit)
        })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelSelection {
    pub provider: JsString,
    pub model: JsString,
    pub api: JsString,
    pub base_url: Option<JsString>,
    pub headers: Option<JsObject>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub message: JsString,
}

/// `/\/+$/` (`llm-extraction.ts:847`).
fn strip_trailing_slash(value: &JsString) -> JsString {
    let mut end = value.units().len();
    while end > 0 && value.units()[end - 1] == u16::from(b'/') {
        end -= 1;
    }
    JsString::from_utf16(value.units()[..end].to_vec())
}

/// `resolveSelection` (`llm-extraction.ts:2191-2230`).
pub fn resolve_selection(
    config: &JsValue,
    agent_id: Option<&JsString>,
) -> Result<ModelSelection, ConfigError> {
    let parsed = parse_config_model_ref(
        resolve_agent_primary_model(config, agent_id).as_ref(),
        config,
    )
    .ok_or_else(|| ConfigError {
        message: JsString::from("Could not resolve an OpenClaw model for memory extraction"),
    })?;
    let providers = config
        .get("models")
        .and_then(JsRecord::of)
        .and_then(|models| models.get_str("providers").and_then(JsRecord::of));
    let provider = providers
        .and_then(|providers| providers.get(&parsed.provider))
        .and_then(JsRecord::of);
    let model = provider
        .and_then(|provider| provider.get_str("models"))
        .and_then(|models| match models {
            JsValue::Array(items) => items.iter().find(|item| {
                JsRecord::of(item)
                    .and_then(|record| record.get_str("id").and_then(JsValue::as_string))
                    .is_some_and(|id| id == &parsed.model)
            }),
            _ => None,
        })
        .and_then(JsRecord::of);
    let api = model
        .and_then(|model| model.get_str("api").and_then(JsValue::as_string))
        .or_else(|| {
            provider.and_then(|provider| provider.get_str("api").and_then(JsValue::as_string))
        })
        .cloned()
        .unwrap_or_else(|| JsString::from("openai-completions"));
    let base_url = model
        .and_then(|model| model.get_str("baseUrl").and_then(JsValue::as_string))
        .or_else(|| {
            provider.and_then(|provider| provider.get_str("baseUrl").and_then(JsValue::as_string))
        })
        .and_then(|base_url| {
            let trimmed = trimmed(Some(base_url));
            (!trimmed.is_empty()).then(|| strip_trailing_slash(&trimmed))
        });
    let mut headers = JsObject::new();
    for source in [
        provider.and_then(|provider| provider.get_str("headers")),
        model.and_then(|model| model.get_str("headers")),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(source) = sanitize_headers(Some(source)) {
            merge_headers(&mut headers, Some(&source));
        }
    }
    Ok(ModelSelection {
        provider: parsed.provider,
        model: parsed.model,
        api,
        base_url,
        headers: (!headers.is_empty()).then_some(headers),
    })
}

/// `resolveApiKey` (`llm-extraction.ts:2231-2258`). `resolved_auth` is the
/// already-awaited `{ apiKey }` from `runtime.modelAuth`.
pub fn resolve_api_key(
    config: &JsValue,
    provider: &JsString,
    env: &dyn Fn(&str) -> Option<String>,
    resolved_auth: Option<&JsValue>,
) -> Result<JsString, ConfigError> {
    let configured = config
        .get("models")
        .and_then(JsRecord::of)
        .and_then(|models| models.get_str("providers").and_then(JsRecord::of))
        .and_then(|providers| providers.get(provider))
        .and_then(JsRecord::of)
        .and_then(|provider| provider.get_str("apiKey").and_then(JsValue::as_string))
        .map(|api_key| trimmed(Some(api_key)))
        .unwrap_or_default();
    if !configured.is_empty() {
        if looks_like_env_var_name(&configured)
            && let Ok(name) = configured.to_utf8()
            && let Some(value) = env(&name)
        {
            let value = trimmed(Some(&JsString::from(value)));
            if !value.is_empty() {
                return Ok(value);
            }
        }
        return Ok(configured);
    }
    if let Some(api_key) = resolved_auth
        .and_then(|auth| auth.as_object())
        .and_then(|auth| auth.get("apiKey"))
        && super::super::storage::js_truthy(api_key)
    {
        let value = trimmed(Some(&JsString::from_utf16(
            crate::js_value::to_string_units(api_key).unwrap_or_default(),
        )));
        if !value.is_empty() {
            return Ok(value);
        }
    }
    let mut units: Vec<u16> = "No API key resolved for extraction provider \""
        .encode_utf16()
        .collect();
    units.extend(provider.units());
    units.push(u16::from(b'"'));
    Err(ConfigError {
        message: JsString::from_utf16(units),
    })
}

// -- service.ts:331-344 (mergeIndexingSettings) ----------------------------

/// TS numbers stay `f64`: `Math.floor`/`Math.max` preserve `NaN`/`Infinity`.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedIndexingSettings {
    pub reasoning_mode: JsString,
    pub auto_index_interval_minutes: f64,
    pub auto_dream_interval_minutes: f64,
}

pub fn merge_indexing_settings(partial: Option<&JsValue>) -> MergedIndexingSettings {
    let record = partial.and_then(JsValue::as_object);
    MergedIndexingSettings {
        reasoning_mode: match record.and_then(|record| record.get("reasoningMode")) {
            Some(JsValue::String(value)) if value.equals_str("accuracy_first") => {
                JsString::from("accuracy_first")
            }
            _ => JsString::from("answer_first"),
        },
        auto_index_interval_minutes: interval(
            record.and_then(|record| record.get("autoIndexIntervalMinutes")),
            30.0,
        ),
        auto_dream_interval_minutes: interval(
            record.and_then(|record| record.get("autoDreamIntervalMinutes")),
            60.0,
        ),
    }
}

/// `Math.max(0, Math.floor(value))` when `typeof value === "number"`.
fn interval(value: Option<&JsValue>, fallback: f64) -> f64 {
    match value {
        Some(JsValue::Number(value)) if value.is_nan() => f64::NAN,
        Some(JsValue::Number(value)) => value.floor().max(0.0),
        _ => fallback,
    }
}
