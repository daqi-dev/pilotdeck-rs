//! Pure config snapshot diffing and change classification, ported from the
//! fixed TypeScript baseline `src/pilot/config/classifyChanges.ts`.
//!
//! Paths are carried as [`JsString`] rather than Rust `String`: the baseline
//! concatenates JavaScript strings, so a config key containing a lone surrogate
//! must survive into the reported path unchanged.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

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
    diff_values(
        Operand::Value(config_of(previous)),
        Operand::Value(config_of(next)),
        &JsString::default(),
    )
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

static NULL: JsValue = JsValue::Null;

/// Own function members of `Array.prototype` (Node 22). `length` is handled on
/// the array itself and `__proto__` is inherited from `Object.prototype`.
const ARRAY_PROTOTYPE_METHODS: &[&str] = &[
    "constructor",
    "at",
    "concat",
    "copyWithin",
    "fill",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "lastIndexOf",
    "pop",
    "push",
    "reverse",
    "shift",
    "unshift",
    "slice",
    "sort",
    "splice",
    "includes",
    "indexOf",
    "join",
    "keys",
    "entries",
    "values",
    "forEach",
    "filter",
    "flat",
    "flatMap",
    "map",
    "every",
    "some",
    "reduce",
    "reduceRight",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
    "toLocaleString",
    "toString",
];

/// Which prototype supplied an inherited function. `Object.prototype` and
/// `Array.prototype` expose distinct functions under the same names, and
/// `Object.is` distinguishes them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinPrototype {
    Object,
    Array,
}

/// A property read result. The wire model stores JSON values but not the
/// inherited function members of `Object.prototype`/`Array.prototype`, nor
/// `Object.prototype` itself; those are represented here so `diff_values` can
/// apply the baseline's `Object.is` rules to them. Shared with the runtime's
/// provider-count lookup, which needs the same JavaScript index semantics.
#[derive(Clone, Copy)]
pub(crate) enum Operand<'a> {
    Value(&'a JsValue),
    Number(f64),
    Function(BuiltinPrototype),
    ObjectPrototype,
}

impl<'a> Operand<'a> {
    fn as_array(self) -> Option<&'a [JsValue]> {
        match self {
            Self::Value(JsValue::Array(items)) => Some(items),
            _ => None,
        }
    }
    fn entries(self) -> &'a [(JsString, JsValue)] {
        match self {
            Self::Value(JsValue::Object(object)) => object.entries(),
            _ => &[],
        }
    }
    fn is_diffable_object(self) -> bool {
        matches!(
            self,
            Self::Value(JsValue::Object(_)) | Self::ObjectPrototype
        )
    }
}

fn diff_values(left: Operand<'_>, right: Operand<'_>, prefix: &JsString) -> Vec<JsString> {
    if operand_is(left, right) {
        return vec![];
    }

    if let (Some(left), Some(right)) = (left.as_array(), right.as_array()) {
        if left.len() != right.len() {
            return vec![path_or_root(prefix)];
        }
        let mut changes = vec![];
        for (index, (left, right)) in left.iter().zip(right).enumerate() {
            changes.extend(diff_values(
                Operand::Value(left),
                Operand::Value(right),
                &index_path(prefix, index),
            ));
        }
        return changes;
    }

    if !left.is_diffable_object() || !right.is_diffable_object() {
        return vec![path_or_root(prefix)];
    }

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
        let left = get_property(left, key).unwrap_or(Operand::Value(&super::value::UNDEFINED));
        let right = get_property(right, key).unwrap_or(Operand::Value(&super::value::UNDEFINED));
        changes.extend(diff_values(left, right, &path));
    }
    changes
}

/// `Object.is` for operands. The wire model carries no reference identity, so
/// containers are always recursed; for the acyclic snapshots this yields the
/// same paths as the baseline's identity check, since equal containers recurse
/// to no changes.
fn operand_is(left: Operand<'_>, right: Operand<'_>) -> bool {
    match (left, right) {
        (Operand::Value(left), Operand::Value(right)) => value_is(left, right),
        (Operand::Number(left), Operand::Number(right)) => number_is(left, right),
        (Operand::Number(left), Operand::Value(JsValue::Number(right)))
        | (Operand::Value(JsValue::Number(right)), Operand::Number(left)) => {
            number_is(left, *right)
        }
        (Operand::Function(left), Operand::Function(right)) => left == right,
        (Operand::ObjectPrototype, Operand::ObjectPrototype) => true,
        _ => false,
    }
}

fn value_is(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(left), JsValue::Bool(right)) => left == right,
        (JsValue::Number(left), JsValue::Number(right)) => number_is(*left, *right),
        (JsValue::String(left), JsValue::String(right)) => left == right,
        _ => false,
    }
}

/// `Object.is` for numbers: NaN equals NaN and `+0`/`-0` differ.
fn number_is(left: f64, right: f64) -> bool {
    (left.is_nan() && right.is_nan())
        || (left == right && (left != 0.0 || left.is_sign_negative() == right.is_sign_negative()))
}

/// Property read following the baseline's JavaScript lookup: own entries, the
/// explicit prototype chain, then the builtin members of `Object.prototype`
/// (and `Array.prototype` when an array appears in the chain). A `None`
/// prototype means the implicit `Object.prototype`; an explicit `Null`
/// prototype (for example `Object.create(null)`) has no builtin fallback.
/// Shared with the runtime's provider-count lookup rather than duplicated.
pub(crate) fn get_property<'a>(operand: Operand<'a>, key: &JsString) -> Option<Operand<'a>> {
    match operand {
        Operand::Value(JsValue::Object(object)) => object_property(object, key),
        Operand::ObjectPrototype => object_prototype_property(key),
        _ => None,
    }
}

fn object_property<'a>(receiver: &'a JsObject, key: &JsString) -> Option<Operand<'a>> {
    let mut current = Some(receiver);
    let mut array_prototype = false;
    while let Some(object) = current {
        if let Some(value) = object.get_key(key) {
            return Some(Operand::Value(value));
        }
        match object.prototype() {
            None => break,
            Some(JsValue::Object(prototype)) => current = Some(prototype),
            Some(JsValue::Array(items)) => {
                if key.equals_str("length") {
                    return Some(Operand::Number(items.len() as f64));
                }
                // ECMAScript array indices stop at 2^32-2, the same bound as
                // the shared key rule; larger keys are ordinary properties.
                if let Some(index) = key.array_index()
                    && let Some(value) = items.get(index as usize)
                {
                    return Some(Operand::Value(value));
                }
                array_prototype = true;
                current = None;
            }
            Some(_) => return None,
        }
    }
    if key.equals_str("__proto__") {
        return Some(match receiver.prototype() {
            Some(prototype) => Operand::Value(prototype),
            None => Operand::ObjectPrototype,
        });
    }
    if array_prototype && is_array_prototype_method(key) {
        return Some(Operand::Function(BuiltinPrototype::Array));
    }
    if is_object_prototype_function(key) {
        return Some(Operand::Function(BuiltinPrototype::Object));
    }
    None
}

/// `Object.prototype` has no enumerable own keys; reading `__proto__` yields
/// its own null prototype, and the named members are functions.
fn object_prototype_property<'a>(key: &JsString) -> Option<Operand<'a>> {
    if key.equals_str("__proto__") {
        return Some(Operand::Value(&NULL));
    }
    is_object_prototype_function(key).then_some(Operand::Function(BuiltinPrototype::Object))
}

fn is_object_prototype_function(key: &JsString) -> bool {
    !key.equals_str("__proto__") && super::value::builtin_property(key)
}

fn is_array_prototype_method(key: &JsString) -> bool {
    ARRAY_PROTOTYPE_METHODS
        .iter()
        .any(|name| key.equals_str(name))
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
