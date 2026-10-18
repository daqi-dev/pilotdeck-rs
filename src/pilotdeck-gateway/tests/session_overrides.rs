//! Session permission registries and creation overrides replayed from the real
//! TypeScript baseline (`tests/fixtures/session-overrides/generate.mts`).
//! Every recorded outcome, size and state snapshot comes from the baseline
//! classes; Rust does not hard-code expected values.
use pilotdeck_gateway::config::permission::{
    AllowRulesHandle, ConfiguredRuleSet, GatewaySessionPermissionModeRegistry,
    GatewaySessionPermissionRuleSetRegistry, Lease, PermissionMode,
};
use pilotdeck_gateway::config::session_overrides::SessionConfigOverrides;
use pilotdeck_gateway::config::session_value::SessionValue;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
    collections::{HashMap, HashSet},
    fs,
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/session-overrides"
);
const SOURCE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";
const RULE_SET_SOURCE: &str = "src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts";
const MODE_SOURCE: &str = "src/gateway/permission/GatewaySessionPermissionModeRegistry.ts";
const OVERRIDES_SOURCE: &str = "src/always-on/runtime/SessionConfigOverrides.ts";
const RULE_SET_SHA256: &str = "9939624f8adf49c9d1a5dbea2077c745dc790c39dd3e8acb52e934e33a88d589";
const MODE_SHA256: &str = "0ad34fcc5a0347dcc18dae3452d0fdb8cdfef9132e3f62849fe51232c46b9af0";
const OVERRIDES_SHA256: &str = "a951cf48d225a7dd85cb2de8a36e6629ac43e2a9f288042ff13fb29f15f385b5";
static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value
        .as_object()
        .and_then(|object| object.get_property(key))
        .unwrap_or(&UNDEFINED)
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("string")
        .to_utf8()
        .expect("fixture text is valid UTF-8")
}

fn string(value: &JsValue) -> JsString {
    value.as_string().expect("string").clone()
}

fn array<'a>(value: &'a JsValue, name: &str) -> &'a [JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("{name} must be an array"),
    }
}

/// `oracle.json` encodes JavaScript-only values (`undefined`, `NaN`, `-0`, ...)
/// as a single-key `{"$": tag}` object. The generator rejects a real fixture
/// object carrying a `$` key.
fn decode(value: &JsValue) -> JsValue {
    let JsValue::Object(object) = value else {
        return match value {
            JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
            other => other.clone(),
        };
    };
    if let Some(tag) = object.get("$").and_then(JsValue::as_string)
        && object.len() == 1
    {
        return match tag.to_utf8().expect("tag").as_str() {
            "undefined" => JsValue::Undefined,
            "NaN" => JsValue::Number(f64::NAN),
            "Infinity" => JsValue::Number(f64::INFINITY),
            "-Infinity" => JsValue::Number(f64::NEG_INFINITY),
            "-0" => JsValue::Number(-0.0),
            other => panic!("unknown value tag {other}"),
        };
    }
    let mut decoded = JsObject::new();
    for (key, value) in object.entries() {
        decoded.insert(key.clone(), decode(value));
    }
    JsValue::Object(decoded)
}

fn rules_to_value(rules: &[SessionValue]) -> JsValue {
    JsValue::Array(rules.iter().map(SessionValue::to_value).collect())
}

fn empty() -> JsValue {
    JsValue::Object(JsObject::new())
}

fn pair(value: &JsValue) -> (String, JsString) {
    let items = array(value, "pair");
    (text(&items[0]), string(&items[1]))
}

fn store_get(
    stores: &HashMap<String, SessionConfigOverrides>,
    store: &str,
    key: &JsString,
) -> JsValue {
    stores
        .get(store)
        .and_then(|overrides| overrides.get(key))
        .map(|v| v.to_value())
        .unwrap_or(JsValue::Undefined)
}

fn store_rules(
    stores: &HashMap<String, SessionConfigOverrides>,
    store: &str,
    key: &JsString,
) -> Option<ConfiguredRuleSet> {
    stores
        .get(store)
        .and_then(|overrides| overrides.permission_rules(key))
}

fn mode_value(mode: Option<PermissionMode>) -> JsValue {
    mode.map(|mode| JsValue::String(mode.as_js_string().clone()))
        .unwrap_or(JsValue::Undefined)
}

fn oracle() -> JsValue {
    let text = fs::read_to_string(format!("{FIXTURES}/oracle.json")).expect("oracle.json");
    assert!(
        !text.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    parse_json(&text).expect("oracle.json parses")
}

fn assert_sources(oracle: &JsValue) {
    assert_eq!(text(field(oracle, "baselineCommit")), SOURCE_COMMIT);
    let hashes = field(oracle, "sourceSha256");
    assert_eq!(text(field(hashes, RULE_SET_SOURCE)), RULE_SET_SHA256);
    assert_eq!(text(field(hashes, MODE_SOURCE)), MODE_SHA256);
    assert_eq!(text(field(hashes, OVERRIDES_SOURCE)), OVERRIDES_SHA256);
}

#[test]
fn real_ts_rule_set_registry_matches_outcomes_and_size() {
    let oracle = oracle();
    assert_sources(&oracle);
    let steps = array(field(&oracle, "ruleSet"), "ruleSet");
    assert!(steps.len() >= 40, "need the full real TS rule-set sequence");

    let registry = GatewaySessionPermissionRuleSetRegistry::new();
    let mut leases: HashMap<String, Lease> = HashMap::new();
    let mut lease_labels: Vec<String> = vec![];
    let mut labels = HashSet::new();
    let mut keys: Vec<JsString> = vec![];

    for entry in steps {
        let step = field(entry, "step");
        let op = text(field(step, "op"));
        if let Some(key) = field(step, "key").as_string()
            && !keys.contains(key)
        {
            keys.push(key.clone());
        }
        let outcome = match op.as_str() {
            "size" => JsValue::Number(registry.size() as f64),
            "acquire" => {
                let label = text(field(step, "lease"));
                assert!(labels.insert(label.clone()), "duplicate lease {label}");
                lease_labels.push(label.clone());
                let key = string(field(step, "key"));
                let configured = match decode(field(step, "configured")) {
                    JsValue::Undefined => None,
                    value => Some(ConfiguredRuleSet::from_permission_rules(&value)),
                };
                let lease = registry.acquire(&key, configured.as_ref());
                let rules = lease.rules().to_value();
                let mut result = JsObject::new();
                result.insert("rules".into(), rules);
                leases.insert(label, lease);
                JsValue::Object(result)
            }
            "release" => {
                let label = text(field(step, "lease"));
                leases
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown lease {label}"))
                    .release();
                let mut result = JsObject::new();
                result.insert("released".into(), JsValue::Bool(true));
                JsValue::Object(result)
            }
            "leaseRules" => {
                let label = text(field(step, "lease"));
                let rules = leases
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown lease {label}"))
                    .rules()
                    .to_value();
                let mut result = JsObject::new();
                result.insert("rules".into(), rules);
                JsValue::Object(result)
            }
            "grant" => JsValue::Bool(registry.grant(
                &string(field(step, "key")),
                SessionValue::from(decode(field(step, "rule"))),
            )),
            "allow" => registry.allow_rules(&string(field(step, "key"))).to_value(),
            "updateAsk" => {
                let push = array(field(step, "push"), "push")
                    .iter()
                    .map(|v| SessionValue::from(decode(v)))
                    .collect::<Vec<_>>();
                let mut before = None;
                let mut after = None;
                let updated = registry.update_ask_rules(&string(field(step, "key")), |ask| {
                    before = Some(ask.clone());
                    ask.extend(push.clone());
                    after = Some(ask.clone());
                });
                let mut result = JsObject::new();
                result.insert("updated".into(), JsValue::Bool(updated));
                result.insert(
                    "askBefore".into(),
                    before
                        .map(|rules| rules_to_value(&rules))
                        .unwrap_or(JsValue::Null),
                );
                result.insert(
                    "askAfter".into(),
                    after
                        .map(|rules| rules_to_value(&rules))
                        .unwrap_or(JsValue::Null),
                );
                JsValue::Object(result)
            }
            "close" => {
                registry.close_session(&string(field(step, "key")));
                empty()
            }
            "dispose" => {
                registry.dispose();
                empty()
            }
            other => panic!("unknown rule-set step {other}"),
        };
        assert_eq!(outcome, decode(field(entry, "outcome")), "outcome: {op}");
        assert_eq!(
            registry.size() as f64,
            field(entry, "size").as_number().expect("size"),
            "size after {op}"
        );

        let mut allow = JsObject::new();
        for key in &keys {
            allow.insert(key.clone(), registry.allow_rules(key).to_value());
        }
        let mut live = JsObject::new();
        for label in &lease_labels {
            let rules = leases
                .get(label)
                .unwrap_or_else(|| panic!("unknown lease {label}"))
                .rules()
                .to_value();
            live.insert(label.as_str().into(), rules);
        }
        let mut state = JsObject::new();
        state.insert("allow".into(), JsValue::Object(allow));
        state.insert("leases".into(), JsValue::Object(live));
        assert_eq!(
            JsValue::Object(state),
            decode(field(entry, "state")),
            "state after {op}"
        );
    }
}

#[test]
fn real_ts_mode_registry_matches_transitions_and_state() {
    let oracle = oracle();
    assert_sources(&oracle);
    let steps = array(field(&oracle, "mode"), "mode");
    assert!(steps.len() >= 30, "need the full real TS mode sequence");

    let mut registry = GatewaySessionPermissionModeRegistry::new();
    let mut keys: Vec<JsString> = vec![];

    for entry in steps {
        let step = field(entry, "step");
        let op = text(field(step, "op"));
        if let Some(key) = field(step, "key").as_string()
            && !keys.contains(key)
        {
            keys.push(key.clone());
        }
        let key = field(step, "key").as_string().cloned();
        let outcome = match op.as_str() {
            "get" => mode_value(registry.get(key.as_ref().expect("mode key"))),
            "getBase" => mode_value(registry.get_base(key.as_ref().expect("mode key"))),
            "set" => {
                registry.set(
                    key.as_ref().expect("mode key"),
                    PermissionMode::from(string(field(step, "mode"))),
                );
                empty()
            }
            "transition" => {
                let fallback = match field(step, "fallbackBase") {
                    JsValue::Undefined => None,
                    value => Some(PermissionMode::from(string(value))),
                };
                registry.transition(
                    key.as_ref().expect("mode key"),
                    PermissionMode::from(string(field(step, "mode"))),
                    fallback,
                );
                empty()
            }
            "clear" => {
                registry.clear(key.as_ref().expect("mode key"));
                empty()
            }
            "dispose" => {
                registry.dispose();
                empty()
            }
            other => panic!("unknown mode step {other}"),
        };
        assert_eq!(outcome, decode(field(entry, "outcome")), "outcome: {op}");

        let mut state = JsObject::new();
        for key in &keys {
            let mut value = JsObject::new();
            value.insert("current".into(), mode_value(registry.get(key)));
            value.insert("base".into(), mode_value(registry.get_base(key)));
            state.insert(key.clone(), JsValue::Object(value));
        }
        assert_eq!(
            JsValue::Object(state),
            decode(field(entry, "state")),
            "state after {op}"
        );
    }
}

#[test]
fn real_ts_session_config_overrides_match_spread_and_state() {
    let oracle = oracle();
    assert_sources(&oracle);
    let steps = array(field(&oracle, "overrides"), "overrides");
    assert!(steps.len() >= 35, "need the full real TS override sequence");

    let mut overrides = SessionConfigOverrides::new();
    let mut keys: Vec<JsString> = vec![];

    for entry in steps {
        let step = field(entry, "step");
        let op = text(field(step, "op"));
        if let Some(key) = field(step, "key").as_string()
            && !keys.contains(key)
        {
            keys.push(key.clone());
        }
        let key = field(step, "key").as_string().cloned();
        let mutate = |value: &SessionValue| {
            for (name, patch) in field(step, "mutate").as_object().expect("mutate").entries() {
                value.set(name.clone(), decode(patch));
            }
        };
        let outcome = match op.as_str() {
            "set" => {
                overrides.set(
                    key.as_ref().expect("override key"),
                    &SessionValue::from(decode(field(step, "value"))),
                );
                empty()
            }
            "setMutate" => {
                let input = SessionValue::from(decode(field(step, "value")));
                overrides.set(key.as_ref().expect("override key"), &input);
                mutate(&input);
                overrides
                    .get(key.as_ref().expect("override key"))
                    .unwrap()
                    .to_value()
            }
            "getMutate" => {
                let returned = overrides.get(key.as_ref().expect("override key")).unwrap();
                mutate(&returned);
                overrides
                    .get(key.as_ref().expect("override key"))
                    .unwrap()
                    .to_value()
            }
            "get" => overrides
                .get(key.as_ref().expect("override key"))
                .map(|v| v.to_value())
                .unwrap_or(JsValue::Undefined),
            "delete" => {
                overrides.delete(key.as_ref().expect("override key"));
                empty()
            }
            "deletePrefix" => {
                overrides.delete_prefix(&string(field(step, "prefix")));
                empty()
            }
            "clear" => {
                overrides.clear();
                empty()
            }
            other => panic!("unknown override step {other}"),
        };
        assert_eq!(outcome, decode(field(entry, "outcome")), "outcome: {op}");

        let mut state = JsObject::new();
        for key in &keys {
            state.insert(
                key.clone(),
                overrides
                    .get(key)
                    .map(|v| v.to_value())
                    .unwrap_or(JsValue::Undefined),
            );
        }
        assert_eq!(
            JsValue::Object(state),
            decode(field(entry, "state")),
            "state after {op}"
        );
    }
}

/// The factory hands `override.permissionRules` to `acquire`, so the override
/// object and the live registry arrays must be one shared state. This replays
/// the real TS combination: grant → override view → recreate → grant again,
/// plus cross-generation release and `updateAskRules` propagation.
#[test]
fn real_ts_combined_overrides_and_live_rule_set_share_state() {
    let oracle = oracle();
    assert_sources(&oracle);
    let steps = array(field(&oracle, "combined"), "combined");
    assert!(steps.len() >= 40, "need the full real TS combined sequence");

    let mut overrides = SessionConfigOverrides::new();
    let registry = GatewaySessionPermissionRuleSetRegistry::new();
    let mut leases: HashMap<String, Lease> = HashMap::new();
    let mut allow_views: HashMap<String, AllowRulesHandle> = HashMap::new();
    let mut allow_view_labels: Vec<String> = vec![];
    let mut lease_labels: Vec<String> = vec![];
    let mut labels = HashSet::new();
    let mut keys: Vec<JsString> = vec![];

    for entry in steps {
        let step = field(entry, "step");
        let op = text(field(step, "op"));
        let key = field(step, "key").as_string().cloned();
        if let Some(key) = &key
            && !keys.contains(key)
        {
            keys.push(key.clone());
        }
        let outcome = match op.as_str() {
            "set" => {
                overrides.set(
                    key.as_ref().expect("combined key"),
                    &SessionValue::from(decode(field(step, "value"))),
                );
                empty()
            }
            "get" => overrides
                .get(key.as_ref().expect("combined key"))
                .map(|v| v.to_value())
                .unwrap_or(JsValue::Undefined),
            "setFromGet" => {
                let key = key.as_ref().expect("combined key");
                let value = overrides.get(key).unwrap_or_else(SessionValue::object);
                for (name, patch) in field(step, "patch").as_object().expect("patch").entries() {
                    value.set(name.clone(), decode(patch));
                }
                overrides.set(key, &value);
                overrides
                    .get(key)
                    .map(|v| v.to_value())
                    .unwrap_or(JsValue::Undefined)
            }
            "acquire" => {
                let label = text(field(step, "lease"));
                assert!(labels.insert(label.clone()), "duplicate lease {label}");
                lease_labels.push(label.clone());
                let key = key.as_ref().expect("combined key").clone();
                let configured = overrides.permission_rules(&key);
                let lease = registry.acquire(&key, configured.as_ref());
                let mut result = JsObject::new();
                result.insert("rules".into(), lease.rules().to_value());
                lease_labels.push(label.clone());
                leases.insert(label, lease);
                JsValue::Object(result)
            }
            "leaseRules" => {
                let label = text(field(step, "lease"));
                let rules = leases
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown lease {label}"))
                    .rules()
                    .to_value();
                let mut result = JsObject::new();
                result.insert("rules".into(), rules);
                JsValue::Object(result)
            }
            "grant" => JsValue::Bool(registry.grant(
                key.as_ref().expect("combined key"),
                SessionValue::from(decode(field(step, "rule"))),
            )),
            "allow" => registry
                .allow_rules(key.as_ref().expect("combined key"))
                .to_value(),
            "holdAllow" => {
                let label = text(field(step, "view"));
                assert!(
                    !allow_views.contains_key(&label),
                    "duplicate allow view {label}"
                );
                allow_view_labels.push(label.clone());
                allow_views.insert(
                    label,
                    registry.allow_rules(key.as_ref().expect("combined key")),
                );
                empty()
            }
            "readHeldAllow" => {
                let label = text(field(step, "view"));
                allow_views
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown held allow view {label}"))
                    .to_value()
            }
            "updateAsk" => {
                let push = array(field(step, "push"), "push")
                    .iter()
                    .map(|v| SessionValue::from(decode(v)))
                    .collect::<Vec<_>>();
                let mut before = None;
                let mut after = None;
                let updated =
                    registry.update_ask_rules(key.as_ref().expect("combined key"), |ask| {
                        before = Some(ask.clone());
                        ask.extend(push.clone());
                        after = Some(ask.clone());
                    });
                let mut result = JsObject::new();
                result.insert("updated".into(), JsValue::Bool(updated));
                result.insert(
                    "askBefore".into(),
                    before
                        .map(|rules| rules_to_value(&rules))
                        .unwrap_or(JsValue::Null),
                );
                result.insert(
                    "askAfter".into(),
                    after
                        .map(|rules| rules_to_value(&rules))
                        .unwrap_or(JsValue::Null),
                );
                JsValue::Object(result)
            }
            "close" => {
                registry.close_session(key.as_ref().expect("combined key"));
                empty()
            }
            "release" => {
                let label = text(field(step, "lease"));
                leases
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown lease {label}"))
                    .release();
                let mut result = JsObject::new();
                result.insert("released".into(), JsValue::Bool(true));
                JsValue::Object(result)
            }
            "dispose" => {
                registry.dispose();
                empty()
            }
            other => panic!("unknown combined step {other}"),
        };
        assert_eq!(outcome, decode(field(entry, "outcome")), "outcome: {op}");
        assert_eq!(
            registry.size() as f64,
            field(entry, "size").as_number().expect("size"),
            "size after {op}"
        );

        let mut overrides_state = JsObject::new();
        for key in &keys {
            overrides_state.insert(
                key.clone(),
                overrides
                    .get(key)
                    .map(|v| v.to_value())
                    .unwrap_or(JsValue::Undefined),
            );
        }
        let mut allow = JsObject::new();
        for key in &keys {
            allow.insert(key.clone(), registry.allow_rules(key).to_value());
        }
        let mut held = JsObject::new();
        for label in &allow_view_labels {
            held.insert(
                label.as_str().into(),
                allow_views
                    .get(label)
                    .unwrap_or_else(|| panic!("unknown held allow view {label}"))
                    .to_value(),
            );
        }
        let mut live = JsObject::new();
        for label in &lease_labels {
            let rules = leases
                .get(label)
                .unwrap_or_else(|| panic!("unknown lease {label}"))
                .rules()
                .to_value();
            live.insert(label.as_str().into(), rules);
        }
        let mut state = JsObject::new();
        state.insert("overrides".into(), JsValue::Object(overrides_state));
        state.insert("allow".into(), JsValue::Object(allow));
        state.insert("held".into(), JsValue::Object(held));
        state.insert("leases".into(), JsValue::Object(live));
        assert_eq!(
            JsValue::Object(state),
            decode(field(entry, "state")),
            "state after {op}"
        );
    }
}

/// codex round-2 P1 sequences: the baseline's `get` returns the exact nested
/// `permissionRules` object, so one view can be re-used across `set`s, stores,
/// and a `delete`. The fixture records both values and reference comparisons;
/// the Rust replay asserts [`ConfiguredRuleSet::same_object`] for the latter.
#[test]
fn real_ts_permission_rules_views_keep_object_identity() {
    let oracle = oracle();
    assert_sources(&oracle);
    let steps = array(field(&oracle, "identity"), "identity");
    assert!(steps.len() >= 55, "need the full real TS identity sequence");

    let mut stores: HashMap<String, SessionConfigOverrides> = HashMap::new();
    let registry = GatewaySessionPermissionRuleSetRegistry::new();
    let mut leases: HashMap<String, Lease> = HashMap::new();
    let mut lease_labels: Vec<String> = vec![];
    let mut views: HashMap<String, SessionValue> = HashMap::new();
    let mut store_keys: Vec<(String, JsString)> = vec![];
    let mut session_keys: Vec<JsString> = vec![];
    let mut labels = HashSet::new();

    for entry in steps {
        let step = field(entry, "step");
        let op = text(field(step, "op"));
        let store = field(step, "store")
            .as_string()
            .map(|value| value.to_utf8().expect("store name"));
        let key = field(step, "key").as_string().cloned();
        if let (Some(store), Some(key)) = (&store, &key)
            && !store_keys
                .iter()
                .any(|(name, stored)| name == store && stored == key)
        {
            store_keys.push((store.clone(), key.clone()));
        }
        if let Some(session) = field(step, "session").as_string()
            && !session_keys.contains(session)
        {
            session_keys.push(session.clone());
        }
        let outcome = match op.as_str() {
            "set" => {
                stores
                    .entry(store.clone().expect("identity store"))
                    .or_default()
                    .set(
                        key.as_ref().expect("identity key"),
                        &SessionValue::from(decode(field(step, "value"))),
                    );
                empty()
            }
            "get" => {
                let store = store.as_ref().expect("identity store");
                let key = key.as_ref().expect("identity key");
                let view = stores
                    .get(store)
                    .and_then(|s| s.get(key))
                    .expect("held view");
                if let Some(label) = field(step, "hold").as_string() {
                    let label = label.to_utf8().expect("hold label");
                    views.insert(label, view.clone());
                }
                view.to_value()
            }
            "setFromView" => {
                let label = text(field(step, "view"));
                let view = views
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown held view {label}"))
                    .clone();
                stores
                    .entry(store.clone().expect("identity store"))
                    .or_default()
                    .set(key.as_ref().expect("identity key"), &view);
                empty()
            }
            "setFromViewPatch" => {
                let label = text(field(step, "view"));
                let view = views.get(&label).expect("held view").spread();
                for (name, patch) in field(step, "patch").as_object().expect("patch").entries() {
                    view.set(name.clone(), decode(patch));
                }
                stores
                    .entry(store.clone().expect("identity store"))
                    .or_default()
                    .set(key.as_ref().expect("identity key"), &view);
                empty()
            }
            "sameStores" => {
                let (left_store, left_key) = pair(field(step, "left"));
                let (right_store, right_key) = pair(field(step, "right"));
                let same = match (
                    store_rules(&stores, &left_store, &left_key),
                    store_rules(&stores, &right_store, &right_key),
                ) {
                    (Some(left), Some(right)) => left.same_object(&right),
                    _ => false,
                };
                JsValue::Bool(same)
            }
            "sameHeld" => {
                let label = text(field(step, "view"));
                let view = views
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown held view {label}"));
                JsValue::Bool(
                    stores
                        .get(store.as_ref().expect("identity store"))
                        .is_some_and(|overrides| {
                            overrides
                                .same_permission_rules(key.as_ref().expect("identity key"), view)
                        }),
                )
            }
            "delete" => {
                if let Some(overrides) = stores.get_mut(store.as_ref().expect("identity store")) {
                    overrides.delete(key.as_ref().expect("identity key"));
                }
                empty()
            }
            "acquire" => {
                let label = text(field(step, "lease"));
                assert!(labels.insert(label.clone()), "duplicate lease {label}");
                let configured = store_rules(
                    &stores,
                    store.as_ref().expect("identity store"),
                    key.as_ref().expect("identity key"),
                );
                let lease = registry.acquire(
                    field(step, "session")
                        .as_string()
                        .expect("identity session"),
                    configured.as_ref(),
                );
                let mut result = JsObject::new();
                result.insert("rules".into(), lease.rules().to_value());
                lease_labels.push(label.clone());
                leases.insert(label, lease);
                JsValue::Object(result)
            }
            "leaseRules" => {
                let label = text(field(step, "lease"));
                let rules = leases
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown lease {label}"))
                    .rules()
                    .to_value();
                let mut result = JsObject::new();
                result.insert("rules".into(), rules);
                JsValue::Object(result)
            }
            "grant" => JsValue::Bool(
                registry.grant(
                    field(step, "session")
                        .as_string()
                        .expect("identity session"),
                    SessionValue::from(decode(field(step, "rule"))),
                ),
            ),
            "allow" => registry
                .allow_rules(
                    field(step, "session")
                        .as_string()
                        .expect("identity session"),
                )
                .to_value(),
            "updateAsk" => {
                let push = array(field(step, "push"), "push")
                    .iter()
                    .map(|v| SessionValue::from(decode(v)))
                    .collect::<Vec<_>>();
                let mut before = None;
                let mut after = None;
                let updated = registry.update_ask_rules(
                    field(step, "session")
                        .as_string()
                        .expect("identity session"),
                    |ask| {
                        before = Some(ask.clone());
                        ask.extend(push.clone());
                        after = Some(ask.clone());
                    },
                );
                let mut result = JsObject::new();
                result.insert("updated".into(), JsValue::Bool(updated));
                result.insert(
                    "askBefore".into(),
                    before
                        .map(|rules| rules_to_value(&rules))
                        .unwrap_or(JsValue::Null),
                );
                result.insert(
                    "askAfter".into(),
                    after
                        .map(|rules| rules_to_value(&rules))
                        .unwrap_or(JsValue::Null),
                );
                JsValue::Object(result)
            }
            "close" => {
                registry.close_session(
                    field(step, "session")
                        .as_string()
                        .expect("identity session"),
                );
                empty()
            }
            "release" => {
                let label = text(field(step, "lease"));
                leases
                    .get(&label)
                    .unwrap_or_else(|| panic!("unknown lease {label}"))
                    .release();
                let mut result = JsObject::new();
                result.insert("released".into(), JsValue::Bool(true));
                JsValue::Object(result)
            }
            "dispose" => {
                registry.dispose();
                empty()
            }
            other => panic!("unknown identity step {other}"),
        };
        assert_eq!(outcome, decode(field(entry, "outcome")), "outcome: {op}");
        assert_eq!(
            registry.size() as f64,
            field(entry, "size").as_number().expect("size"),
            "size after {op}"
        );

        let mut store_state = JsObject::new();
        for (store, key) in &store_keys {
            let label = format!("{store}:{}", key.to_utf8().expect("store key"));
            store_state.insert(label.as_str().into(), store_get(&stores, store, key));
        }
        let mut allow = JsObject::new();
        for key in &session_keys {
            allow.insert(key.clone(), registry.allow_rules(key).to_value());
        }
        let mut live = JsObject::new();
        for label in &lease_labels {
            let rules = leases
                .get(label)
                .unwrap_or_else(|| panic!("unknown lease {label}"))
                .rules()
                .to_value();
            live.insert(label.as_str().into(), rules);
        }
        let mut state = JsObject::new();
        state.insert("stores".into(), JsValue::Object(store_state));
        state.insert("allow".into(), JsValue::Object(allow));
        state.insert("leases".into(), JsValue::Object(live));
        assert_eq!(
            JsValue::Object(state),
            decode(field(entry, "state")),
            "state after {op}"
        );
    }
}

/// The recorded graph distinguishes object containers, arrays and elements.
/// No expected identity or mutation result is reconstructed from Rust logic.
#[test]
fn real_ts_shared_object_graph_and_lease_mutation() {
    let oracle = oracle();
    assert_sources(&oracle);
    let mut values: HashMap<String, SessionValue> = HashMap::new();
    let mut stores: HashMap<String, SessionConfigOverrides> = HashMap::new();
    let mut keys: HashMap<String, Vec<JsString>> = HashMap::new();
    let mut value_order = Vec::new();
    let mut store_order = Vec::new();
    let mut lease_order = Vec::new();
    let registry = GatewaySessionPermissionRuleSetRegistry::new();
    let mut leases: HashMap<String, Lease> = HashMap::new();
    fn at(values: &HashMap<String, SessionValue>, path: &JsValue) -> SessionValue {
        let path = array(path, "path");
        let mut value = values.get(&text(&path[0])).expect("known value").clone();
        for name in &path[1..] {
            value = value.get(&text(name)).expect("known property");
        }
        value
    }
    for (index, entry) in array(field(&oracle, "sharing"), "sharing")
        .iter()
        .enumerate()
    {
        let step = field(entry, "step");
        let op = text(field(step, "op"));
        if let Some(name) = field(step, "name").as_string() {
            let name = name.to_utf8().unwrap();
            if !value_order.contains(&name) {
                value_order.push(name);
            }
        }
        if op == "acquire" {
            lease_order.push(text(field(step, "lease")));
        }
        let mut outcome = empty();
        match op.as_str() {
            "value" => {
                values.insert(
                    text(field(step, "name")),
                    decode(field(step, "value")).into(),
                );
            }
            "alias" | "spread" => {
                let value = at(&values, field(step, "source"));
                values.insert(
                    text(field(step, "name")),
                    if op == "spread" {
                        value.spread()
                    } else {
                        value
                    },
                );
            }
            "setProperty" => {
                let value = if field(step, "source").is_undefined() {
                    decode(field(step, "value")).into()
                } else {
                    at(&values, field(step, "source"))
                };
                at(&values, field(step, "target")).set(string(field(step, "property")), value);
            }
            "push" => {
                let value = at(&values, field(step, "source"));
                at(&values, field(step, "target")).update_array(|items| items.push(value));
            }
            "leasePush" => {
                let value = at(&values, field(step, "source"));
                leases[&text(field(step, "lease"))]
                    .rules()
                    .update_member(&text(field(step, "member")), |items| items.push(value));
            }
            "remove" => {
                at(&values, field(step, "target")).update_array(|items| {
                    items.remove(field(step, "index").as_number().unwrap() as usize);
                });
            }
            "storeSet" => {
                let store = text(field(step, "store"));
                let key = string(field(step, "key"));
                if !store_order.contains(&store) {
                    store_order.push(store.clone());
                }
                let store_keys = keys.entry(store.clone()).or_default();
                if !store_keys.contains(&key) {
                    store_keys.push(key.clone());
                }
                stores
                    .entry(store)
                    .or_default()
                    .set(&key, &at(&values, field(step, "source")));
            }
            "storeGet" => {
                let value = stores[&text(field(step, "store"))]
                    .get(&string(field(step, "key")))
                    .unwrap();
                values.insert(text(field(step, "name")), value);
            }
            "delete" => stores
                .get_mut(&text(field(step, "store")))
                .unwrap()
                .delete(&string(field(step, "key"))),
            "clear" => stores.get_mut(&text(field(step, "store"))).unwrap().clear(),
            "acquire" => {
                let configured = (!field(step, "source").is_undefined())
                    .then(|| ConfiguredRuleSet::from_shared(at(&values, field(step, "source"))));
                leases.insert(
                    text(field(step, "lease")),
                    registry.acquire(&string(field(step, "key")), configured.as_ref()),
                );
            }
            "lease" => {
                values.insert(
                    text(field(step, "name")),
                    leases[&text(field(step, "lease"))].rules().value(),
                );
            }
            "allow" => {
                values.insert(
                    text(field(step, "name")),
                    registry.allow_rules(&string(field(step, "key"))).value(),
                );
            }
            "grant" => {
                outcome = JsValue::Bool(registry.grant(
                    &string(field(step, "key")),
                    at(&values, field(step, "source")),
                ));
            }
            "same" => {
                outcome = JsValue::Bool(
                    at(&values, field(step, "left"))
                        .same_object(&at(&values, field(step, "right"))),
                );
            }
            "release" => leases[&text(field(step, "lease"))].release(),
            "close" => registry.close_session(&string(field(step, "key"))),
            "dispose" => registry.dispose(),
            other => panic!("unknown sharing operation {other}"),
        }
        let mut store_state = JsObject::new();
        for store in &store_order {
            let store_keys = &keys[store];
            let mut state = JsObject::new();
            for key in store_keys {
                state.insert(
                    key.clone(),
                    stores[store]
                        .get(key)
                        .map(|v| v.to_value())
                        .unwrap_or(JsValue::Undefined),
                );
            }
            store_state.insert(store.as_str().into(), JsValue::Object(state));
        }
        let mut value_state = JsObject::new();
        for name in &value_order {
            value_state.insert(name.as_str().into(), values[name].to_value());
        }
        let mut lease_state = JsObject::new();
        for name in &lease_order {
            lease_state.insert(name.as_str().into(), leases[name].rules().to_value());
        }
        let mut state = JsObject::new();
        state.insert("values".into(), JsValue::Object(value_state));
        state.insert("stores".into(), JsValue::Object(store_state));
        state.insert("leases".into(), JsValue::Object(lease_state));
        assert_eq!(
            outcome,
            decode(field(entry, "outcome")),
            "sharing #{index} {op} outcome"
        );
        assert_eq!(
            registry.size() as f64,
            field(entry, "size").as_number().unwrap(),
            "sharing #{index} {op} size"
        );
        assert_eq!(
            JsValue::Object(state),
            decode(field(entry, "state")),
            "sharing #{index} {op} state"
        );
    }
}
