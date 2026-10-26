//! Real TS oracle for A6.6-edgeclaw-5 LLM config resolution.
use pilotdeck_gateway::runtime::memory::edgeclaw::llm::config::{
    ConfigError, LlmOptions, MergedIndexingSettings, ModelSelection, OpenClawModelConfig,
    OpenClawStore, build_llm_config_with, looks_like_env_var_name, memory_api_type_for_protocol,
    merge_indexing_settings, parse_config_model_ref, parse_model_ref, resolve_agent_primary_model,
    resolve_api_key, resolve_memory_llm, resolve_selection, sanitize_headers,
};
use pilotdeck_gateway::runtime::memory::{MemoryConfig, MemoryModelConfig, MemoryModelProvider};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

const ORACLE: &str = include_str!("fixtures/edgeclaw-llm-config/oracle.json");

fn oracle() -> JsValue {
    baseline::oracle(ORACLE);
    parse_json(ORACLE).unwrap()
}

fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("expected an array, got {other:?}"),
    }
}

fn object(entries: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}

/// JSON oracle values use `$undefined` / `$number` sentinels.
fn decode(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
        JsValue::Object(object) => {
            if object.len() == 1 && matches!(object.get("$undefined"), Some(JsValue::Bool(true))) {
                return JsValue::Undefined;
            }
            if object.len() == 1
                && let Some(JsValue::String(kind)) = object.get("$number")
                && let Ok(kind) = kind.to_utf8()
            {
                return JsValue::Number(match kind.as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    "-0" => -0.0,
                    other => other.parse().expect("number sentinel"),
                });
            }
            let mut decoded = JsObject::new();
            for (key, value) in object.entries() {
                decoded.insert(key.clone(), decode(value));
            }
            JsValue::Object(decoded)
        }
        other => other.clone(),
    }
}

fn wire(value: &JsValue) -> String {
    match stringify_json(value).unwrap() {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn assert_wire(actual: &JsValue, expected: &JsValue) {
    assert_eq!(wire(actual), wire(expected), "actual {actual:?}");
}

fn opt_string(value: &JsValue) -> Option<JsString> {
    value.as_string().cloned()
}

fn string_value(value: &Option<JsString>) -> JsValue {
    value
        .clone()
        .map(JsValue::String)
        .unwrap_or(JsValue::Undefined)
}

fn env_map(value: &JsValue) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(object) = value.as_object() {
        for (key, value) in object.entries() {
            env.insert(
                key.to_utf8().expect("env key"),
                value.as_string().expect("env value").to_utf8().unwrap(),
            );
        }
    }
    env
}

fn state_kind(state: &JsValue) -> String {
    state
        .get("kind")
        .and_then(JsValue::as_string)
        .expect("state kind")
        .to_utf8()
        .unwrap()
}

fn apply_state(home: &Path, state: &JsValue) {
    let path = home.join(".openclaw/openclaw.json");
    if state_kind(state) == "missing" {
        let _ = fs::remove_file(path);
        return;
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        state
            .get("text")
            .and_then(JsValue::as_string)
            .expect("state text")
            .to_utf8()
            .unwrap(),
    )
    .unwrap();
}

fn home_for(scratch: &scratch::Scratch, index: usize) -> PathBuf {
    let home = scratch.path().join(format!("case-{index}"));
    fs::create_dir_all(&home).unwrap();
    home
}

fn open_claw_json(config: &OpenClawModelConfig) -> JsValue {
    let mut output = object([
        ("provider", JsValue::String(config.provider.clone())),
        ("model", JsValue::String(config.model.clone())),
        ("apiType", string_value(&config.api_type)),
        ("baseUrl", string_value(&config.base_url)),
        ("apiKey", string_value(&config.api_key)),
    ]);
    if let Some(headers) = &config.headers {
        output.insert("headers".into(), JsValue::Object(headers.clone()));
    }
    JsValue::Object(output)
}

fn llm_json(options: &LlmOptions) -> JsValue {
    let mut output = object([
        (
            "provider",
            JsValue::String(options.provider.clone().unwrap_or_default()),
        ),
        (
            "model",
            JsValue::String(options.model.clone().unwrap_or_default()),
        ),
        ("baseUrl", string_value(&options.base_url)),
        ("apiKey", string_value(&options.api_key)),
    ]);
    if let Some(api_type) = &options.api_type {
        output.insert("apiType".into(), JsValue::String(api_type.clone()));
    }
    JsValue::Object(output)
}

fn selection_json(selection: &ModelSelection) -> JsValue {
    let mut output = object([
        ("provider", JsValue::String(selection.provider.clone())),
        ("model", JsValue::String(selection.model.clone())),
        ("api", JsValue::String(selection.api.clone())),
    ]);
    if let Some(base_url) = &selection.base_url {
        output.insert("baseUrl".into(), JsValue::String(base_url.clone()));
    }
    if let Some(headers) = &selection.headers {
        output.insert("headers".into(), JsValue::Object(headers.clone()));
    }
    JsValue::Object(output)
}

fn merged_json(settings: &MergedIndexingSettings) -> JsValue {
    JsValue::Object(object([
        (
            "reasoningMode",
            JsValue::String(settings.reasoning_mode.clone()),
        ),
        (
            "autoIndexIntervalMinutes",
            JsValue::Number(settings.auto_index_interval_minutes),
        ),
        (
            "autoDreamIntervalMinutes",
            JsValue::Number(settings.auto_dream_interval_minutes),
        ),
    ]))
}

fn error_json(error: &ConfigError) -> JsValue {
    JsValue::Object(object([("$error", JsValue::String(error.message.clone()))]))
}

fn options_from(value: &JsValue) -> Option<LlmOptions> {
    let object = value.as_object()?;
    Some(LlmOptions {
        provider: object.get("provider").and_then(opt_string),
        model: object.get("model").and_then(opt_string),
        model_ref: object.get("modelRef").and_then(opt_string),
        api_type: object.get("apiType").and_then(opt_string),
        base_url: object.get("baseUrl").and_then(opt_string),
        api_key: object.get("apiKey").and_then(opt_string),
        headers: object.get("headers").and_then(JsValue::as_object).cloned(),
    })
}

fn memory_config_from(value: &JsValue) -> MemoryConfig {
    let mut config = MemoryConfig::new(true, "edgeclaw");
    config.model = value.get("model").and_then(opt_string);
    config.api_type = value.get("apiType").and_then(opt_string);
    config
}

fn model_config_from(value: &JsValue) -> Option<MemoryModelConfig> {
    let providers = value.get("providers")?.as_object()?;
    let mut config = MemoryModelConfig::default();
    for (name, provider) in providers.entries() {
        let Some(provider) = provider.as_object() else {
            continue;
        };
        config.providers.insert(
            name.to_utf8().expect("provider name"),
            MemoryModelProvider {
                protocol: provider.get("protocol").and_then(opt_string),
                url: provider.get("url").and_then(opt_string),
                api_key: provider.get("apiKey").and_then(opt_string),
            },
        );
    }
    Some(config)
}

fn observed(case: &JsValue, scratch: &scratch::Scratch, index: usize) -> JsValue {
    let name = case
        .get("fn")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    let args = decode(case.get("args").unwrap());
    let arg = |index: usize| &items(&args)[index];
    match name.as_str() {
        "buildLlmConfig" => {
            let options = options_from(arg(0));
            let env = env_map(arg(1));
            let home = home_for(scratch, index);
            apply_state(&home, arg(2));
            let store = OpenClawStore::new();
            let open_claw = store.resolve_in(&home);
            build_llm_config_with(options.as_ref(), open_claw.as_ref(), &|key| {
                env.get(key).cloned()
            })
        }
        "resolveOpenClawModelConfig" => {
            let home = home_for(scratch, index);
            let store = OpenClawStore::new();
            let mut results = Vec::new();
            for state in items(arg(0)) {
                apply_state(&home, state);
                results.push(
                    store
                        .resolve_in(&home)
                        .as_ref()
                        .map(open_claw_json)
                        .unwrap_or(JsValue::Null),
                );
            }
            JsValue::Array(results)
        }
        "sanitizeHeaders" => sanitize_headers(Some(arg(0)))
            .map(JsValue::Object)
            .unwrap_or(JsValue::Undefined),
        "resolveAgentPrimaryModel" => resolve_agent_primary_model(
            arg(0),
            (!matches!(arg(1), JsValue::Undefined)).then(|| arg(1).as_string().unwrap()),
        )
        .map(JsValue::String)
        .unwrap_or(JsValue::Undefined),
        "parseExtractorModelRef" => parse_config_model_ref(
            (!matches!(arg(0), JsValue::Undefined)).then(|| arg(0).as_string().unwrap()),
            arg(1),
        )
        .map(|parsed| {
            JsValue::Object(object([
                ("provider", JsValue::String(parsed.provider)),
                ("model", JsValue::String(parsed.model)),
            ]))
        })
        .unwrap_or(JsValue::Undefined),
        "looksLikeEnvVarName" => {
            JsValue::Bool(looks_like_env_var_name(arg(0).as_string().unwrap()))
        }
        "resolveSelection" => {
            let agent_id =
                (!matches!(arg(1), JsValue::Undefined)).then(|| arg(1).as_string().unwrap());
            match resolve_selection(arg(0), agent_id) {
                Ok(selection) => selection_json(&selection),
                Err(error) => error_json(&error),
            }
        }
        "resolveApiKey" => {
            let env = env_map(arg(2));
            let auth = match arg(3) {
                JsValue::Undefined | JsValue::Null => None,
                value => Some(value),
            };
            match resolve_api_key(
                arg(0),
                arg(1).as_string().unwrap(),
                &|key| env.get(key).cloned(),
                auth,
            ) {
                Ok(api_key) => JsValue::String(api_key),
                Err(error) => error_json(&error),
            }
        }
        "mergeIndexingSettings" => merged_json(&merge_indexing_settings(
            (!matches!(arg(0), JsValue::Undefined)).then(|| arg(0)),
        )),
        "resolveMemoryLlm" => {
            let config = memory_config_from(arg(0));
            let model_config = model_config_from(arg(1));
            let agent_model = (!matches!(arg(2), JsValue::Undefined))
                .then(|| arg(2).as_string().unwrap().to_utf8().unwrap());
            resolve_memory_llm(&config, model_config.as_ref(), agent_model.as_deref())
                .as_ref()
                .map(llm_json)
                .unwrap_or(JsValue::Undefined)
        }
        "memoryApiTypeForProtocol" => memory_api_type_for_protocol(
            (!matches!(arg(0), JsValue::Undefined)).then(|| arg(0).as_string().unwrap()),
        )
        .map(JsValue::String)
        .unwrap_or(JsValue::Undefined),
        other => panic!("oracle has an unknown function {other}"),
    }
}

#[test]
fn llm_config_matches_ts() {
    let oracle = oracle();
    let scratch = scratch::Scratch::new("edgeclaw-llm-config");
    let mut counts = BTreeMap::new();
    for (index, case) in items(oracle.get("cases").unwrap()).iter().enumerate() {
        let name = case
            .get("fn")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        *counts.entry(name.clone()).or_insert(0usize) += 1;
        let actual = observed(case, &scratch, index);
        let expected = decode(case.get("expected").unwrap());
        assert_wire(&actual, &expected);
        if name == "resolveMemoryLlm" {
            assert_eq!(
                matches!(expected, JsValue::Undefined),
                matches!(actual, JsValue::Undefined),
                "resolveMemoryLlm undefined arity"
            );
        }
    }
    assert_eq!(counts.len(), 11, "oracle function coverage {counts:?}");
}

#[test]
fn parse_model_ref_keeps_empty_provider() {
    let parsed = parse_model_ref(Some(&JsString::from("/model")));
    assert_eq!(parsed.provider, Some(JsString::from("")));
    assert_eq!(parsed.model, Some(JsString::from("model")));
}

#[test]
fn open_claw_cache_does_not_reprobe_home_after_first_read() {
    let scratch = scratch::Scratch::new("edgeclaw-llm-config-cache");
    let home = scratch.path().join("home");
    fs::create_dir_all(home.join(".openclaw")).unwrap();
    fs::write(
        home.join(".openclaw/openclaw.json"),
        r#"{"models":{"providers":{"alpha":{"models":[{"id":"m1"}]}}}}"#,
    )
    .unwrap();
    let store = OpenClawStore::new();
    let first = store.resolve(|| Some(home.clone())).expect("first read");
    let second = store
        .resolve(|| -> Option<PathBuf> { panic!("home must not be re-read") })
        .expect("cached");
    assert_eq!(first, second);
}

#[test]
#[ignore]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-llm-config");
}
