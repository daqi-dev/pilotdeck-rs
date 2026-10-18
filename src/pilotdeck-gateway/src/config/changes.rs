//! Pure config snapshot diffing and change classification, ported from the
//! fixed TypeScript baseline `src/pilot/config/classifyChanges.ts`.
//!
//! Paths are carried as [`JsString`] rather than Rust `String`: the baseline
//! concatenates JavaScript strings, so a config key containing a lone surrogate
//! must survive into the reported path unchanged.
use pilotdeck_protocol::{JsString, JsValue};

/// `PilotConfigChangeClass` without the baseline's unused `"invalid"` member:
/// `classifyPath` never returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeClass {
    RuntimeLive,
    NextRequest,
    NextRuntime,
    RestartRequired,
}

impl ChangeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeLive => "runtime-live",
            Self::NextRequest => "next-request",
            Self::NextRuntime => "next-runtime",
            Self::RestartRequired => "restart-required",
        }
    }
}

/// `diffConfigSnapshots`: only the `config` member of each snapshot is compared.
pub fn diff_config_snapshots(previous: &JsValue, next: &JsValue) -> Vec<JsString> {
    diff_values(config_of(previous), config_of(next), &JsString::default())
}

/// `classifyConfigChanges`: one class per distinct class, first-seen order.
pub fn classify_config_changes(changed_paths: &[JsString]) -> Vec<ChangeClass> {
    let mut classes = Vec::new();
    for path in changed_paths {
        let class = classify_path(path);
        if !classes.contains(&class) {
            classes.push(class);
        }
    }
    classes
}

/// `isRunPolicyOnlyChange`: false for an empty list, otherwise every path must
/// be `runPolicy` or start with `runPolicy.`.
pub fn is_run_policy_only_change(changed_paths: &[JsString]) -> bool {
    !changed_paths.is_empty() && changed_paths.iter().all(is_run_policy_path)
}

fn config_of(snapshot: &JsValue) -> &JsValue {
    snapshot
        .as_object()
        .and_then(|object| object.get_property("config"))
        .unwrap_or(&super::value::UNDEFINED)
}

fn classify_path(path: &JsString) -> ChangeClass {
    if starts_with(path, "agent.") || starts_with(path, "model.") {
        return ChangeClass::NextRequest;
    }
    if path.equals_str("extension.includeHookEvents") {
        return ChangeClass::RuntimeLive;
    }
    if starts_with(path, "extension.") {
        return ChangeClass::NextRuntime;
    }
    if starts_with(path, "router.") {
        return classify_router_path(path);
    }
    if starts_with(path, "alwaysOn.") {
        return ChangeClass::NextRuntime;
    }
    if starts_with(path, "cron.") {
        return ChangeClass::NextRuntime;
    }
    if starts_with(path, "tools.") {
        return ChangeClass::NextRuntime;
    }
    if path.equals_str("modules") || starts_with(path, "modules.") {
        return ChangeClass::RestartRequired;
    }
    if starts_with(path, "proxy.") || path.equals_str("proxy") {
        return ChangeClass::RuntimeLive;
    }
    if is_run_policy_path(path) {
        return ChangeClass::NextRequest;
    }
    ChangeClass::NextRuntime
}

fn classify_router_path(path: &JsString) -> ChangeClass {
    if starts_with(path, "router.scenarios.")
        || starts_with(path, "router.fallback.")
        || starts_with(path, "router.tokenSaver.tiers.")
        || path.equals_str("router.zeroUsageRetry.maxAttempts")
        || path.equals_str("router.zeroUsageRetry.enabled")
    {
        return ChangeClass::NextRequest;
    }
    if starts_with(path, "router.tokenSaver.judge") {
        return ChangeClass::RuntimeLive;
    }
    if path.equals_str("router.autoOrchestrate.skillExtensionId") {
        return ChangeClass::NextRuntime;
    }
    if path.equals_str("router.stats.enabled") || path.equals_str("router.customRouter.extensionId")
    {
        return ChangeClass::RestartRequired;
    }
    ChangeClass::NextRuntime
}

fn is_run_policy_path(path: &JsString) -> bool {
    path.equals_str("runPolicy") || starts_with(path, "runPolicy.")
}

fn starts_with(path: &JsString, prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    path.units().starts_with(&prefix)
}

fn diff_values(left: &JsValue, right: &JsValue, prefix: &JsString) -> Vec<JsString> {
    if object_is(left, right) {
        return vec![];
    }

    if let (JsValue::Array(left), JsValue::Array(right)) = (left, right) {
        if left.len() != right.len() {
            return vec![path_or_root(prefix)];
        }
        let mut changes = vec![];
        for (index, (left, right)) in left.iter().zip(right).enumerate() {
            changes.extend(diff_values(left, right, &index_path(prefix, index)));
        }
        return changes;
    }

    if !is_diffable_object(left) || !is_diffable_object(right) {
        return vec![path_or_root(prefix)];
    }

    let (Some(left), Some(right)) = (left.as_object(), right.as_object()) else {
        unreachable!("is_diffable_object checked both sides");
    };
    let mut keys: Vec<&JsString> = left
        .entries()
        .iter()
        .chain(right.entries())
        .map(|(key, _)| key)
        .collect();
    // JavaScript's default sort is by UTF-16 code unit; `JsString`'s derived
    // `Ord` over its UTF-16 units is exactly that order.
    keys.sort_unstable();
    keys.dedup();

    let mut changes = vec![];
    for key in keys {
        let path = child_path(prefix, key);
        let left = get_property(left, key).unwrap_or(&super::value::UNDEFINED);
        let right = get_property(right, key).unwrap_or(&super::value::UNDEFINED);
        changes.extend(diff_values(left, right, &path));
    }
    changes
}

/// `Object.is`: NaN equals NaN and `+0`/`-0` differ. The wire model carries no
/// reference identity, so containers are always recursed; for the acyclic JSON
/// snapshots this yields the same paths as the baseline's identity check, since
/// equal containers recurse to no changes.
fn object_is(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(left), JsValue::Bool(right)) => left == right,
        (JsValue::Number(left), JsValue::Number(right)) => {
            (left.is_nan() && right.is_nan())
                || (left == right
                    && (*left != 0.0 || left.is_sign_negative() == right.is_sign_negative()))
        }
        (JsValue::String(left), JsValue::String(right)) => left == right,
        _ => false,
    }
}

fn is_diffable_object(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_))
}

/// Own property first, then the explicit prototype chain (the config loader
/// models literal `__proto__` keys that way). There is no implicit
/// `Object.prototype` in the wire model; keys stay `JsString` so lone
/// surrogates are not decoded.
fn get_property<'a>(
    object: &'a pilotdeck_protocol::JsObject,
    key: &JsString,
) -> Option<&'a JsValue> {
    let mut object = Some(object);
    while let Some(current) = object {
        if let Some(value) = current.get_key(key) {
            return Some(value);
        }
        object = current.prototype().and_then(JsValue::as_object);
    }
    None
}

fn path_or_root(prefix: &JsString) -> JsString {
    if prefix.is_empty() {
        JsString::from("<root>")
    } else {
        prefix.clone()
    }
}

fn child_path(prefix: &JsString, key: &JsString) -> JsString {
    let mut units = Vec::with_capacity(prefix.len_utf16() + 1 + key.len_utf16());
    units.extend_from_slice(prefix.units());
    if !prefix.is_empty() {
        units.push(u16::from(b'.'));
    }
    units.extend_from_slice(key.units());
    JsString::from_utf16(units)
}

fn index_path(prefix: &JsString, index: usize) -> JsString {
    let mut units = Vec::with_capacity(prefix.len_utf16() + 8);
    units.extend_from_slice(prefix.units());
    units.push(u16::from(b'['));
    units.extend(index.to_string().encode_utf16());
    units.push(u16::from(b']'));
    JsString::from_utf16(units)
}
