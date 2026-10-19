//! Replays the real TypeScript SDK session recording
//! (`fixtures/sdk-session/oracle.json`) against `runtime::sdk_session`.
//!
//! Every result/state object is rebuilt in the generator's key order and
//! compared as serialized JSON bytes (no key sorting). The fixture was
//! produced by `generate.mts`; its `sourceSha256` covers the import closure of
//! `ProjectRuntimeRegistry.ts` and `sdkSessionSettings.ts`.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    config::{Env, session_overrides::SessionConfigOverrides},
    runtime::{
        plugins::{
            LoadedPlugin, PluginRuntime, PluginRuntimeOptions, PluginSkillContribution,
            PluginSource, loader::LoadedPluginCommand,
        },
        sdk_session::{
            self, ModelField, ModelRef, OrganizationPolicy, OrganizationSettingsPolicy,
            ProviderPolicy, ResolvedSdkRuntime, SdkAbortSignal, SdkPluginLoader,
            SdkRuntimeResolver, SdkSessionError, SdkSessionOptions, SdkSessionRegistry,
            SelectorKind, SelectorPolicy, TaskBudgetStorePort,
        },
    },
    storage::TaskBudgetStore,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const ORACLE: &str = include_str!("fixtures/sdk-session/oracle.json");

fn obj(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

fn array(items: Vec<JsValue>) -> JsValue {
    JsValue::Array(items)
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.into())
}

fn js_string(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

fn undefined_tag() -> JsValue {
    obj(vec![("$", text("undefined"))])
}

/// Decode the fixture's tagged values into real `JsValue`s. `<ROOT>` is
/// replaced with this run's scratch root in every string.
fn decode(value: &JsValue, root: &Path) -> JsValue {
    decode_at(value, root, 0)
}

/// The fixture is configuration-shaped; the cap makes the bound explicit
/// instead of relying on the fixture staying shallow.
fn decode_at(value: &JsValue, root: &Path, depth: usize) -> JsValue {
    assert!(depth <= 64, "fixture value nesting is bounded");
    match value {
        JsValue::String(value) => {
            let text = js_string(value).replace("<ROOT>", &root.to_string_lossy());
            JsValue::String(text.into())
        }
        JsValue::Array(items) => array(
            items
                .iter()
                .map(|item| decode_at(item, root, depth + 1))
                .collect(),
        ),
        JsValue::Object(object) => {
            if object.len() == 1 {
                let tag = object.get("$").and_then(JsValue::as_string);
                if let Some(tag) = tag {
                    if tag.equals_str("undefined") {
                        return JsValue::Undefined;
                    }
                    if tag.equals_str("NaN") {
                        return JsValue::Number(f64::NAN);
                    }
                    if tag.equals_str("Infinity") {
                        return JsValue::Number(f64::INFINITY);
                    }
                    if tag.equals_str("-Infinity") {
                        return JsValue::Number(f64::NEG_INFINITY);
                    }
                    if tag.equals_str("-0") {
                        return JsValue::Number(-0.0);
                    }
                }
            }
            let mut out = JsObject::new();
            for (key, item) in object.entries() {
                out.insert(key.clone(), decode_at(item, root, depth + 1));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

/// Rebuild the explicit `{ entries: [[key, node], ...] }` form in source order.
/// Ordinary JSON decoding cannot preserve a pre-enumeration insertion order for
/// integer-like keys because `encode` has already applied JS key order.
fn ordered_value(value: &JsValue) -> JsValue {
    let Some(JsValue::Array(entries)) = value.get("entries") else {
        return value.clone();
    };
    let mut out = JsObject::new();
    for entry in entries {
        let JsValue::Array(pair) = entry else {
            continue;
        };
        let Some(key) = pair.first().and_then(JsValue::as_string) else {
            continue;
        };
        let Some(node) = pair.get(1) else {
            continue;
        };
        out.insert(key.clone(), ordered_value(node));
    }
    JsValue::Object(out)
}

/// Encode a real value exactly like the generator's `encode`, replacing this
/// run's root with `<ROOT>`.
fn encode(value: &JsValue, root: &Path) -> JsValue {
    encode_at(value, root, 0)
}

fn encode_at(value: &JsValue, root: &Path, depth: usize) -> JsValue {
    assert!(depth <= 64, "fixture value nesting is bounded");
    match value {
        JsValue::Undefined => undefined_tag(),
        JsValue::Null | JsValue::Bool(_) => value.clone(),
        JsValue::Number(number) => {
            if number.is_nan() {
                obj(vec![("$", text("NaN"))])
            } else if *number == f64::INFINITY {
                obj(vec![("$", text("Infinity"))])
            } else if *number == f64::NEG_INFINITY {
                obj(vec![("$", text("-Infinity"))])
            } else if *number == 0.0 && number.is_sign_negative() {
                obj(vec![("$", text("-0"))])
            } else {
                value.clone()
            }
        }
        JsValue::String(value) => {
            let text = js_string(value).replace(&root.to_string_lossy().into_owned(), "<ROOT>");
            JsValue::String(text.into())
        }
        JsValue::Array(items) => array(
            items
                .iter()
                .map(|item| encode_at(item, root, depth + 1))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut out = JsObject::new();
            for (key, item) in object.entries() {
                out.insert(key.clone(), encode_at(item, root, depth + 1));
            }
            JsValue::Object(out)
        }
    }
}

fn assert_same(actual: &JsValue, expected: &JsValue, label: &str) {
    let actual = stringify_json(actual).ok().flatten().unwrap_or_default();
    let expected = stringify_json(expected).ok().flatten().unwrap_or_default();
    assert_eq!(actual, expected, "{label}");
}

fn ok(value: JsValue, root: &Path) -> JsValue {
    obj(vec![
        ("ok", JsValue::Bool(true)),
        ("value", encode(&value, root)),
    ])
}

fn error(error: &SdkSessionError, root: &Path) -> JsValue {
    obj(vec![
        ("ok", JsValue::Bool(false)),
        (
            "error",
            obj(vec![
                ("name", text(&error.name)),
                (
                    "code",
                    if error.code.is_empty() {
                        undefined_tag()
                    } else {
                        text(&error.code)
                    },
                ),
                (
                    "message",
                    encode(&JsValue::String(error.message.as_str().into()), root),
                ),
            ]),
        ),
    ])
}

fn ok_or_error(result: Result<JsValue, SdkSessionError>, root: &Path) -> JsValue {
    match result {
        Ok(value) => ok(value, root),
        Err(error) => self::error(&error, root),
    }
}

fn bool_ok(value: bool, root: &Path) -> JsValue {
    ok(JsValue::Bool(value), root)
}

fn undefined_ok(root: &Path) -> JsValue {
    ok(JsValue::Undefined, root)
}

fn case_string(case: &JsValue, key: &str) -> Option<String> {
    case.get(key).and_then(JsValue::as_string).map(js_string)
}

fn string_list(value: &JsValue) -> Vec<String> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(JsValue::as_string)
            .map(js_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn array_of(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => &[],
    }
}

fn skill_list(value: &JsValue) -> Vec<String> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(|item| item.get("name"))
            .filter_map(JsValue::as_string)
            .map(js_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn selector_policy(value: &JsValue) -> SelectorPolicy {
    SelectorPolicy {
        allow: string_list(value.get("allow").unwrap_or(&JsValue::Undefined)),
        deny: string_list(value.get("deny").unwrap_or(&JsValue::Undefined)),
    }
}

fn provider_policy(value: &JsValue) -> ProviderPolicy {
    ProviderPolicy {
        allow: string_list(value.get("allow").unwrap_or(&JsValue::Undefined)),
        deny: string_list(value.get("deny").unwrap_or(&JsValue::Undefined)),
        origins: selector_policy(value.get("origins").unwrap_or(&JsValue::Undefined)),
        credentials: selector_policy(value.get("credentials").unwrap_or(&JsValue::Undefined)),
    }
}

fn organization_policy(value: &JsValue) -> Option<OrganizationPolicy> {
    if value.is_undefined() {
        return None;
    }
    let mut policy = OrganizationPolicy::default();
    if let Some(models) = value
        .get("models")
        .filter(|value| value.as_object().is_some())
    {
        policy.models = Some(selector_policy(models));
    }
    if let Some(providers) = value
        .get("providers")
        .filter(|value| value.as_object().is_some())
    {
        policy.providers = Some(provider_policy(providers));
    }
    if let Some(sources) = value
        .get("settingSources")
        .filter(|value| value.as_object().is_some())
    {
        policy.setting_sources = Some(selector_policy(sources));
    }
    if let Some(settings) = value
        .get("settings")
        .filter(|value| value.as_object().is_some())
    {
        policy.settings = Some(OrganizationSettingsPolicy {
            session_defaults: settings.get("sessionDefaults").cloned(),
            managed_session_settings: settings.get("managedSessionSettings").cloned(),
            session_default_sources: settings.get("sessionDefaultSources").cloned(),
            enforced_session_settings: settings.get("enforcedSessionSettings").cloned(),
        });
    }
    policy.max_task_budget_usd = value
        .get("limits")
        .and_then(|limits| limits.get("maxTaskBudgetUsd"))
        .and_then(JsValue::as_number);
    Some(policy)
}

fn model_ref(value: &JsValue) -> ModelRef {
    ModelRef {
        provider: value
            .get("provider")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default(),
        model: value
            .get("model")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default(),
    }
}

fn model_field(value: &JsValue) -> ModelField {
    match value.as_string().map(js_string).as_deref() {
        Some("agentModel") => ModelField::AgentModel,
        Some("fallbackModel") => ModelField::FallbackModel,
        Some("subagentModel") => ModelField::SubagentModel,
        _ => ModelField::Model,
    }
}

fn selector_kind(value: &JsValue) -> SelectorKind {
    match value.as_string().map(js_string).as_deref() {
        Some("models") => SelectorKind::Models,
        _ => SelectorKind::Tools,
    }
}

fn write_files(case_dir: &Path, files: &JsValue, root: &Path) {
    let decoded = decode(files, root);
    let JsValue::Array(files) = &decoded else {
        return;
    };
    for file in files {
        let Some(path) = file.get("path").and_then(JsValue::as_string) else {
            continue;
        };
        let Some(content) = file.get("content").and_then(JsValue::as_string) else {
            continue;
        };
        let absolute = case_dir.join(js_string(path));
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(absolute, js_string(content)).unwrap();
    }
}

#[test]
#[should_panic(expected = "negative control")]
fn negative_control_changed_value_is_detected() {
    let actual = obj(vec![("changed", JsValue::Bool(true))]);
    let expected = obj(vec![("changed", JsValue::Bool(false))]);
    assert_same(&actual, &expected, "negative control: changed value");
}

#[test]
#[should_panic(expected = "negative control")]
fn negative_control_error_code_is_detected() {
    let left = SdkSessionError::new("CODE_A", "same");
    let right = SdkSessionError::new("CODE_B", "same");
    assert_same(
        &error(&left, Path::new("/")),
        &error(&right, Path::new("/")),
        "negative control: error code",
    );
}

#[tokio::test]
async fn real_ts_sdk_session_oracle() {
    baseline::oracle(ORACLE);
    let oracle = parse_json(ORACLE).expect("oracle JSON");
    let scratch = scratch::Scratch::new("sdk-session-oracle");
    let root = scratch.path();
    let snapshot = decode(oracle.get("snapshot").expect("snapshot"), root);

    let settings_sources = oracle.get("settingsSources").expect("settingsSources");
    let settings_validate = oracle.get("settingsValidate").expect("settingsValidate");
    let settings_entry_order = oracle
        .get("settingsEntryOrder")
        .expect("settingsEntryOrder");
    let settings_resolve = oracle.get("settingsResolve").expect("settingsResolve");
    let validators = oracle.get("validators").expect("validators");
    let registry = oracle.get("registry").expect("registry");
    for section in [
        settings_sources,
        settings_validate,
        settings_entry_order,
        settings_resolve,
        validators,
        registry,
    ] {
        let cases = section
            .get("cases")
            .and_then(|value| match value {
                JsValue::Array(cases) => Some(cases),
                _ => None,
            })
            .expect("cases array");
        let count = section
            .get("count")
            .and_then(JsValue::as_number)
            .expect("count") as usize;
        assert_eq!(count, cases.len(), "fixture count must be exact");
    }
    let total = oracle
        .get("count")
        .and_then(JsValue::as_number)
        .expect("total count") as usize;
    let sum: usize = [
        settings_sources,
        settings_validate,
        settings_entry_order,
        settings_resolve,
        validators,
        registry,
    ]
    .into_iter()
    .map(|section| section.get("count").and_then(JsValue::as_number).unwrap() as usize)
    .sum();
    assert_eq!(total, sum, "total fixture count");

    // ── settingSources validation ───────────────────────────────────────
    for case in cases(settings_sources) {
        let value = decode(case.get("value").expect("value"), root);
        let actual = match sdk_session::validate_setting_sources(Some(&value)) {
            Ok(()) => undefined_ok(root),
            Err(error) => self::error(&error, root),
        };
        assert_same(
            &actual,
            case.get("result").expect("result"),
            &format!(
                "settingSources:{}",
                case_string(case, "name").unwrap_or_default()
            ),
        );
    }

    // ── settings validation ─────────────────────────────────────────────
    for case in cases(settings_validate) {
        let value = decode(case.get("value").expect("value"), root);
        let actual = match sdk_session::validate_session_settings(Some(&value)) {
            Ok(()) => undefined_ok(root),
            Err(error) => self::error(&error, root),
        };
        assert_same(
            &actual,
            case.get("result").expect("result"),
            &format!(
                "settingsValidate:{}",
                case_string(case, "name").unwrap_or_default()
            ),
        );
    }

    // ── settings validation in source key order ─────────────────────────
    for case in cases(settings_entry_order) {
        let value = ordered_value(case.get("entries").expect("entries"));
        let actual = match sdk_session::validate_session_settings(Some(&value)) {
            Ok(()) => undefined_ok(root),
            Err(error) => self::error(&error, root),
        };
        assert_same(
            &actual,
            case.get("result").expect("result"),
            &format!(
                "settingsEntryOrder:{}",
                case_string(case, "name").unwrap_or_default()
            ),
        );
    }

    // ── settings resolution ─────────────────────────────────────────────
    for case in cases(settings_resolve) {
        let dir = case_string(case, "dir").expect("dir");
        let case_dir = root.join(&dir);
        std::fs::create_dir_all(&case_dir).unwrap();
        for directory in string_list(case.get("dirs").expect("dirs")) {
            std::fs::create_dir_all(case_dir.join(directory)).unwrap();
        }
        write_files(&case_dir, case.get("files").expect("files"), root);
        for link in array_of(case.get("symlinks").expect("symlinks")) {
            let Some(path) = link.get("path").and_then(JsValue::as_string) else {
                continue;
            };
            let Some(target) = link.get("target").and_then(JsValue::as_string) else {
                continue;
            };
            let absolute = case_dir.join(js_string(path));
            if let Some(parent) = absolute.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            #[cfg(unix)]
            symlink(js_string(target), &absolute).unwrap();
        }
        #[cfg(unix)]
        for entry in array_of(case.get("modes").expect("modes")) {
            let Some(path) = entry.get("path").and_then(JsValue::as_string) else {
                continue;
            };
            let Some(mode) = entry.get("mode").and_then(JsValue::as_number) else {
                continue;
            };
            std::fs::set_permissions(
                case_dir.join(js_string(path)),
                std::fs::Permissions::from_mode(mode as u32),
            )
            .unwrap();
        }
        let project_root = decode(case.get("projectRoot").expect("projectRoot"), root);
        let project_root = PathBuf::from(js_string(
            project_root.as_string().expect("projectRoot string"),
        ));
        let decoded_env = decode(case.get("env").expect("env"), root);
        let JsValue::Object(env) = &decoded_env else {
            panic!("env must be an object");
        };
        let mut env_map = Env::new();
        for (key, value) in env.entries() {
            env_map.insert(
                js_string(key),
                js_string(value.as_string().expect("env value")),
            );
        }
        let host_defaults = decode(case.get("hostDefaults").expect("hostDefaults"), root);
        let host_managed = decode(
            case.get("hostManagedSettings")
                .expect("hostManagedSettings"),
            root,
        );
        let host_sources = decode(
            case.get("hostSettingSources").expect("hostSettingSources"),
            root,
        );
        let settings = decode(case.get("settings").expect("settings"), root);
        let setting_sources = decode(case.get("settingSources").expect("settingSources"), root);
        let enforced = decode(
            case.get("hostEnforcedSettings")
                .expect("hostEnforcedSettings"),
            root,
        );
        let actual =
            match sdk_session::resolve_session_settings(sdk_session::ResolveSettingsOptions {
                project_root: &project_root,
                cwd: root,
                env: &env_map,
                homedir: Some(case_dir.as_path()),
                host_defaults: Some(&host_defaults),
                host_managed_settings: Some(&host_managed),
                host_setting_sources: Some(&host_sources),
                settings: Some(&settings),
                setting_sources: Some(&setting_sources),
                host_enforced_settings: Some(&enforced),
            }) {
                Ok(Some(value)) => ok(value, root),
                Ok(None) => undefined_ok(root),
                Err(error) => self::error(&error, root),
            };
        assert_same(
            &actual,
            case.get("result").expect("result"),
            &format!(
                "settingsResolve:{}",
                case_string(case, "name").unwrap_or_default()
            ),
        );
    }

    // ── private validators ──────────────────────────────────────────────
    for case in cases(validators) {
        let kind = case_string(case, "kind").expect("kind");
        let decoded_args = decode(case.get("args").expect("args"), root);
        let JsValue::Array(args) = &decoded_args else {
            panic!("validator args must be an array");
        };
        let actual = match kind.as_str() {
            "managedPermissions" => match sdk_session::validate_managed_permissions(Some(&args[0]))
            {
                Ok(()) => undefined_ok(root),
                Err(error) => self::error(&error, root),
            },
            "managedTools" => {
                match sdk_session::validate_selector_policy(Some(&args[0]), selector_kind(&args[1]))
                {
                    Ok(()) => undefined_ok(root),
                    Err(error) => self::error(&error, root),
                }
            }
            "managedModels" => {
                match sdk_session::validate_selector_policy(Some(&args[0]), selector_kind(&args[1]))
                {
                    Ok(()) => undefined_ok(root),
                    Err(error) => self::error(&error, root),
                }
            }
            "taskBudget" => {
                let mut config = args[0].clone();
                let cap = match &args[1] {
                    JsValue::Number(number) => Some(*number),
                    _ => None,
                };
                sdk_session::apply_organization_task_budget_cap(&mut config, cap);
                ok(config, root)
            }
            "settingSources" => {
                let policy = if args[0].is_undefined() {
                    None
                } else {
                    args[0]
                        .get("settingSources")
                        .filter(|value| value.as_object().is_some())
                        .map(selector_policy)
                };
                match sdk_session::assert_organization_setting_sources_allowed(
                    policy.as_ref(),
                    Some(&args[1]),
                ) {
                    Ok(()) => undefined_ok(root),
                    Err(error) => self::error(&error, root),
                }
            }
            "resolveModel" => {
                let requested = args[0].as_string().expect("requested model");
                match sdk_session::resolve_configured_model(
                    &snapshot,
                    requested,
                    model_field(&args[1]),
                ) {
                    Ok(model) => ok(
                        obj(vec![
                            ("provider", JsValue::String(model.provider)),
                            ("model", JsValue::String(model.model)),
                        ]),
                        root,
                    ),
                    Err(error) => self::error(&error, root),
                }
            }
            "managedModel" => {
                let policy = if args[0].is_undefined() {
                    None
                } else {
                    Some(&args[0])
                };
                match sdk_session::assert_managed_model_allowed(policy, &model_ref(&args[1])) {
                    Ok(()) => undefined_ok(root),
                    Err(error) => self::error(&error, root),
                }
            }
            "orgModel" => {
                let policy = if args[0].is_undefined() {
                    None
                } else {
                    args[0]
                        .get("models")
                        .filter(|value| value.as_object().is_some())
                        .map(selector_policy)
                };
                let model = ModelRef {
                    provider: args[1].as_string().cloned().unwrap_or_default(),
                    model: args[2].as_string().cloned().unwrap_or_default(),
                };
                bool_ok(
                    sdk_session::is_organization_model_allowed(policy.as_ref(), &model),
                    root,
                )
            }
            "providerRejection" => {
                let policy = if args[0].is_undefined() {
                    None
                } else {
                    args[0]
                        .get("providers")
                        .filter(|value| value.as_object().is_some())
                        .map(provider_policy)
                };
                let provider_id = args[1].as_string().expect("provider id");
                let provider = &args[2];
                match sdk_session::organization_provider_policy_rejection(
                    policy.as_ref(),
                    provider_id,
                    Some(provider),
                ) {
                    Some(error) => ok(
                        obj(vec![
                            ("code", text(&error.code)),
                            ("message", text(&error.message)),
                        ]),
                        root,
                    ),
                    None => undefined_ok(root),
                }
            }
            "hasDefaults" => bool_ok(
                organization_policy(&args[0])
                    .as_ref()
                    .is_some_and(OrganizationPolicy::has_session_defaults),
                root,
            ),
            "skills" => {
                let requested = match &args[0] {
                    JsValue::Array(items) => items.as_slice(),
                    _ => &[],
                };
                let available: Vec<PluginSkillContribution> = match &args[1] {
                    JsValue::Array(items) => items
                        .iter()
                        .filter_map(|item| item.get("name"))
                        .filter_map(JsValue::as_string)
                        .map(|name| PluginSkillContribution {
                            name: js_string(name),
                            description: None,
                            path: PathBuf::new(),
                            content: String::new(),
                            namespace: String::new(),
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                ok_or_error(
                    sdk_session::resolve_session_skills(requested, &available),
                    root,
                )
            }
            other => panic!("unknown validator kind {other}"),
        };
        assert_same(
            &actual,
            case.get("result").expect("result"),
            &format!(
                "validator:{}",
                case_string(case, "name").unwrap_or_default()
            ),
        );
    }

    // ── registry operations ─────────────────────────────────────────────
    for case in cases(registry) {
        replay_registry_case(case, &snapshot, root).await;
    }
}

fn cases(section: &JsValue) -> &[JsValue] {
    match section.get("cases") {
        Some(JsValue::Array(cases)) => cases,
        _ => panic!("cases array"),
    }
}

struct FixedResolver {
    project_root: PathBuf,
    snapshot: JsValue,
    plugin_runtime: Arc<PluginRuntime>,
}

impl SdkRuntimeResolver for FixedResolver {
    fn resolve(
        &self,
        _project_key: Option<&JsString>,
    ) -> Result<ResolvedSdkRuntime, SdkSessionError> {
        Ok(ResolvedSdkRuntime {
            project_root: self.project_root.clone(),
            snapshot: self.snapshot.clone(),
            plugin_runtime: Arc::clone(&self.plugin_runtime),
        })
    }
}

struct TestLoader;

impl SdkPluginLoader for TestLoader {
    fn load<'a>(&'a self, path: &'a Path) -> sdk_session::PluginLoadFuture<'a> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = path.to_path_buf();
        Box::pin(async move {
            if name == "plugin-fail" {
                return Err("boom".into());
            }
            let name = if name == "plugin-conflict" {
                "existing".to_string()
            } else {
                name
            };
            Ok(LoadedPlugin {
                name,
                path,
                source: PluginSource::Project,
                commands: Vec::new(),
                skills: Vec::new(),
                output_styles: Vec::new(),
                hooks_config: None,
                mcp_servers: None,
                lsp_servers: None,
                dispose: None,
            })
        })
    }
}

struct SequencedSignal {
    kind: String,
    calls: AtomicUsize,
}

impl SdkAbortSignal for SequencedSignal {
    fn throw_if_aborted(&self) -> Result<(), SdkSessionError> {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let should_throw = (self.kind == "first" && calls >= 1)
            || (self.kind == "second" && calls >= 2)
            || (self.kind == "third" && calls >= 3)
            || (self.kind == "fourth" && calls >= 4);
        if should_throw {
            return Err(SdkSessionError::raw(
                "Error",
                format!("aborted-{}", self.kind),
            ));
        }
        Ok(())
    }
}

fn builtin_plugins(names: &[String], skills: &[String]) -> Vec<Arc<LoadedPlugin>> {
    let command = |name: &str| LoadedPluginCommand {
        name: name.to_string(),
        path: PathBuf::from(format!("/skills/{name}")),
        content: String::new(),
        frontmatter: HashMap::new(),
        is_skill: true,
    };
    let mut plugins = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let skills = if index == 0 {
            skills.iter().map(|skill| command(skill)).collect()
        } else {
            Vec::new()
        };
        plugins.push(Arc::new(LoadedPlugin {
            name: name.clone(),
            path: PathBuf::from(format!("/builtin/{name}")),
            source: PluginSource::Builtin,
            commands: Vec::new(),
            skills,
            output_styles: Vec::new(),
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            dispose: None,
        }));
    }
    if names.is_empty() && !skills.is_empty() {
        plugins.push(Arc::new(LoadedPlugin {
            name: "__skills".into(),
            path: PathBuf::from("/builtin/__skills"),
            source: PluginSource::Builtin,
            commands: Vec::new(),
            skills: skills.iter().map(|skill| command(skill)).collect(),
            output_styles: Vec::new(),
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            dispose: None,
        }));
    }
    plugins
}

fn encoded_state(registry: &SdkSessionRegistry, root: &Path) -> JsValue {
    let configs = array(
        registry
            .config_entries()
            .iter()
            .map(|(key, value)| array(vec![JsValue::String(key.clone()), encode(value, root)]))
            .collect(),
    );
    let plugins = array(
        registry
            .plugin_entries()
            .iter()
            .map(|(key, plugins)| {
                array(vec![
                    JsValue::String(key.clone()),
                    array(plugins.iter().map(|plugin| text(&plugin.name)).collect()),
                ])
            })
            .collect(),
    );
    let projects = array(
        registry
            .project_entries()
            .iter()
            .map(|(key, path)| {
                array(vec![
                    JsValue::String(key.clone()),
                    encode(
                        &JsValue::String(path.to_string_lossy().into_owned().into()),
                        root,
                    ),
                ])
            })
            .collect(),
    );
    let thinking = array(
        registry
            .thinking_entries()
            .iter()
            .map(|(key, value)| array(vec![JsValue::String(key.clone()), encode(value, root)]))
            .collect(),
    );
    let session_key = JsString::from("s");
    let overrides = match registry
        .session_overrides()
        .and_then(|overrides| overrides.get(&session_key))
    {
        Some(value) => array(vec![array(vec![
            JsValue::String(session_key),
            encode(&value.to_value(), root),
        ])]),
        None => array(Vec::new()),
    };
    let handlers = obj(vec![
        (
            "config",
            array(
                registry
                    .handlers()
                    .config_change_sessions()
                    .into_iter()
                    .map(JsValue::String)
                    .collect(),
            ),
        ),
        (
            "hook",
            array(
                registry
                    .handlers()
                    .session_hook_sessions()
                    .into_iter()
                    .map(JsValue::String)
                    .collect(),
            ),
        ),
    ]);
    obj(vec![
        ("configs", configs),
        ("plugins", plugins),
        ("projects", projects),
        ("thinking", thinking),
        ("overrides", overrides),
        ("handlers", handlers),
    ])
}

async fn replay_registry_case(case: &JsValue, snapshot: &JsValue, root: &Path) {
    let name = case_string(case, "name").expect("case name");
    let dir = case_string(case, "dir").expect("case dir");
    let case_dir = root.join(&dir);
    let project_root = case_dir.join("project");
    let fallback_root = case_dir.join("fallback");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::create_dir_all(&fallback_root).unwrap();
    for plugin in ["plugin-a", "plugin-conflict", "plugin-fail"] {
        std::fs::create_dir_all(case_dir.join(plugin)).unwrap();
    }
    write_files(&case_dir, case.get("files").expect("files"), root);

    let plugin_names = string_list(&decode(case.get("pluginNames").expect("pluginNames"), root));
    let plugin_skills = skill_list(&decode(
        case.get("pluginSkills").expect("pluginSkills"),
        root,
    ));
    let plugin_runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: project_root.clone(),
        pilot_home: case_dir.join("home"),
        builtin_skills_root: None,
        builtin_plugins: builtin_plugins(&plugin_names, &plugin_skills),
        builtin_plugins_enabled: HashMap::new(),
    }));
    let policy = organization_policy(&decode(
        case.get("organizationPolicy").expect("organizationPolicy"),
        root,
    ));
    let budget_store = TaskBudgetStore::new(case_dir.join("home"), None);
    let mut env = Env::new();
    env.insert("HOME".into(), case_dir.to_string_lossy().into_owned());
    env.insert(
        "PILOT_HOME".into(),
        case_dir.join("home").to_string_lossy().into_owned(),
    );
    let options = SdkSessionOptions::new(
        fallback_root,
        &case_dir,
        env,
        Arc::new(FixedResolver {
            project_root: project_root.clone(),
            snapshot: snapshot.clone(),
            plugin_runtime,
        }),
    )
    .with_homedir(case_dir.as_path())
    .with_organization_policy(policy)
    .with_plugin_loader(Arc::new(TestLoader))
    .with_task_budget(Arc::new(TaskBudgetStorePort::new(budget_store)))
    .with_clock(Arc::new(|| 0.0_f64));
    let mut registry =
        SdkSessionRegistry::new(options).with_overrides(SessionConfigOverrides::new());
    let mut config_tokens = Vec::new();
    let mut hook_tokens = Vec::new();

    let JsValue::Array(steps) = case.get("steps").expect("steps") else {
        panic!("steps array");
    };
    for (index, step) in steps.iter().enumerate() {
        let label = format!("registry:{name}:step{index}");
        let op = case_string(step, "op").expect("op");
        let input = decode(step.get("input").expect("input"), root);
        let session_key = input
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| JsString::from("s"));
        let actual = match op.as_str() {
            "set" => {
                let config = input.get("config").expect("config").clone();
                let project_key = input
                    .get("projectKey")
                    .and_then(JsValue::as_string)
                    .cloned();
                let signal =
                    input
                        .get("abort")
                        .and_then(JsValue::as_string)
                        .map(|kind| SequencedSignal {
                            kind: js_string(kind),
                            calls: AtomicUsize::new(0),
                        });
                let signal = signal.as_ref().map(|signal| signal as &dyn SdkAbortSignal);
                match registry
                    .set_session_config(&session_key, &config, project_key.as_ref(), signal)
                    .await
                {
                    Ok(changed) => ok(obj(vec![("changed", JsValue::Bool(changed))]), root),
                    Err(error) => self::error(&error, root),
                }
            }
            "assertModel" => {
                let model = input
                    .get("model")
                    .filter(|value| !value.is_undefined())
                    .map(model_ref);
                let project_key = input
                    .get("projectKey")
                    .and_then(JsValue::as_string)
                    .cloned();
                match registry.assert_model_allowed(
                    &session_key,
                    model.as_ref(),
                    project_key.as_ref(),
                ) {
                    Ok(()) => undefined_ok(root),
                    Err(error) => self::error(&error, root),
                }
            }
            "thinking" => {
                let value = input.get("value").expect("value");
                let value = if value.is_undefined() {
                    None
                } else {
                    Some(value)
                };
                registry.set_session_thinking(&session_key, value);
                undefined_ok(root)
            }
            "flag" => {
                let settings = input.get("settings").expect("settings");
                match registry.apply_flag_settings(&session_key, settings) {
                    Ok(result) => ok(
                        obj(vec![
                            (
                                "applied",
                                array(result.applied.iter().map(|value| text(value)).collect()),
                            ),
                            (
                                "cleared",
                                array(result.cleared.iter().map(|value| text(value)).collect()),
                            ),
                        ]),
                        root,
                    ),
                    Err(error) => self::error(&error, root),
                }
            }
            "clear" => match registry.clear_session_state(&session_key) {
                Ok(()) => undefined_ok(root),
                Err(error) => self::error(&error, root),
            },
            "isEphemeral" => bool_ok(registry.is_ephemeral_session(&session_key), root),
            "registerConfig" => {
                let token = registry.register_config_change_handler(&session_key, Arc::new(|_| {}));
                config_tokens.push(token);
                ok(JsValue::Bool(true), root)
            }
            "registerHook" => {
                let token =
                    registry.register_session_hook_handler(&session_key, Arc::new(|_, _| {}));
                hook_tokens.push(token);
                ok(JsValue::Bool(true), root)
            }
            "unregisterConfig" => {
                let index = input
                    .get("token")
                    .and_then(JsValue::as_number)
                    .expect("token") as usize;
                registry
                    .handlers_mut()
                    .unregister_config_change(&config_tokens[index]);
                undefined_ok(root)
            }
            "unregisterHook" => {
                let index = input
                    .get("token")
                    .and_then(JsValue::as_number)
                    .expect("token") as usize;
                registry
                    .handlers_mut()
                    .unregister_session_hook(&hook_tokens[index]);
                undefined_ok(root)
            }
            other => panic!("unknown registry op {other}"),
        };
        assert_same(&actual, step.get("result").expect("result"), &label);
        assert_same(
            &encoded_state(&registry, root),
            step.get("state").expect("state"),
            &format!("{label}:state"),
        );
    }
}
