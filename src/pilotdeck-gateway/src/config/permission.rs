//! Volatile session-scoped permission rules and modes, ported from the fixed
//! TypeScript baseline:
//!
//! - `src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts`
//! - `src/gateway/permission/GatewaySessionPermissionModeRegistry.ts`
//!
//! Both registries own in-memory state only. Nothing here is wired into the
//! Gateway host or a session runtime yet.
//!
//! `PermissionRule` keeps the baseline's four equality fields (`source`,
//! `behavior`, `toolName`, `pattern`) plus the SDK-only `force` marker, which
//! the baseline deliberately ignores when de-duplicating. String members are
//! [`JsString`] because a rule may carry unpaired UTF-16 surrogates from the
//! wire. Source and behavior stay plain strings rather than Rust enums: the
//! baseline validates neither at this layer.

use super::session_value::SessionValue;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

/// One `PermissionRule` object. `pattern`/`force` are optional, matching the
/// baseline's `pattern?` / `force?` fields and `undefined` equality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRule {
    pub source: JsString,
    pub behavior: JsString,
    pub tool_name: JsString,
    pub pattern: Option<JsString>,
    pub force: Option<bool>,
}

impl PermissionRule {
    pub fn new(
        source: impl Into<JsString>,
        behavior: impl Into<JsString>,
        tool_name: impl Into<JsString>,
    ) -> Self {
        Self {
            source: source.into(),
            behavior: behavior.into(),
            tool_name: tool_name.into(),
            pattern: None,
            force: None,
        }
    }

    pub fn with_pattern(mut self, pattern: impl Into<JsString>) -> Self {
        self.pattern = Some(pattern.into());
        self
    }

    pub fn with_force(mut self, force: bool) -> Self {
        self.force = Some(force);
        self
    }

    /// `sameRule`: `force` is intentionally not compared.
    pub fn same_rule(&self, other: &Self) -> bool {
        self.source == other.source
            && self.behavior == other.behavior
            && self.tool_name == other.tool_name
            && self.pattern == other.pattern
    }

    /// Reads one baseline rule object. The baseline stores rule values
    /// verbatim, so a malformed wire object would only surface when
    /// `sameRule` reads a member; the typed port keeps that boundary explicit
    /// instead of panicking: non-objects are skipped and a missing or
    /// non-string required member becomes an empty string.
    pub fn from_value(value: &JsValue) -> Option<Self> {
        let object = value.as_object()?;
        let string = |name: &str| {
            object
                .get(name)
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default()
        };
        Some(Self {
            source: string("source"),
            behavior: string("behavior"),
            tool_name: string("toolName"),
            pattern: object.get("pattern").and_then(JsValue::as_string).cloned(),
            force: object.get("force").and_then(JsValue::as_bool),
        })
    }

    /// Boundary form used by tests and future wire adapters. Property order
    /// follows the baseline object literals (`source`, `behavior`, `toolName`,
    /// then optional `pattern`/`force`).
    pub fn to_value(&self) -> JsValue {
        let mut value = JsObject::new();
        value.insert("source".into(), JsValue::String(self.source.clone()));
        value.insert("behavior".into(), JsValue::String(self.behavior.clone()));
        value.insert("toolName".into(), JsValue::String(self.tool_name.clone()));
        if let Some(pattern) = &self.pattern {
            value.insert("pattern".into(), JsValue::String(pattern.clone()));
        }
        if let Some(force) = self.force {
            value.insert("force".into(), JsValue::Bool(force));
        }
        JsValue::Object(value)
    }
}

/// `PermissionRuleSet`: three mutable arrays owned by one live generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionRuleSet {
    pub allow: Vec<PermissionRule>,
    pub deny: Vec<PermissionRule>,
    pub ask: Vec<PermissionRule>,
}

impl PermissionRuleSet {
    pub fn to_value(&self) -> JsValue {
        rule_set_value(self)
    }
}

fn rule_set_value(rules: &PermissionRuleSet) -> JsValue {
    let mut value = JsObject::new();
    for (name, rules) in [
        ("allow", &rules.allow),
        ("deny", &rules.deny),
        ("ask", &rules.ask),
    ] {
        value.insert(
            name.into(),
            JsValue::Array(rules.iter().map(PermissionRule::to_value).collect()),
        );
    }
    JsValue::Object(value)
}

impl From<PermissionRule> for SessionValue {
    fn from(rule: PermissionRule) -> Self {
        rule.to_value().into()
    }
}

fn same_rule(left: &SessionValue, right: &SessionValue) -> bool {
    ["source", "behavior", "toolName", "pattern"]
        .iter()
        .all(|name| {
            left.get(name)
                .map(|v| v.to_value())
                .unwrap_or(JsValue::Undefined)
                == right
                    .get(name)
                    .map(|v| v.to_value())
                    .unwrap_or(JsValue::Undefined)
        })
}

fn merge_rules(target: &SessionValue, incoming: &SessionValue) {
    // Clone element handles before locking the target: arrays can alias.
    let incoming = incoming.items();
    target.update_array(|target| {
        for rule in incoming {
            if !target.iter().any(|existing| same_rule(existing, &rule)) {
                target.push(rule);
            }
        }
    });
}

fn new_array() -> SessionValue {
    SessionValue::array(vec![])
}

fn snapshot_array(array: &SessionValue) -> Vec<PermissionRule> {
    array
        .items()
        .iter()
        .filter_map(|v| PermissionRule::from_value(&v.to_value()))
        .collect()
}

struct Entry {
    id: u64,
    /// Shared with every lease/handle, so a released lease keeps the exact
    /// arrays it observed even after the final release removes the map entry.
    rules: RuleSetHandle,
    retains: u64,
    /// A Gateway RPC may grant before an AgentHandle has been created.
    grant_pinned: bool,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<JsString, Entry>,
    next_id: u64,
}

impl Inner {
    fn ensure(
        &mut self,
        session_key: &JsString,
        configured: Option<&ConfiguredRuleSet>,
    ) -> (u64, RuleSetHandle) {
        if let Some(entry) = self.entries.get_mut(session_key) {
            if let Some(configured) = configured {
                for name in ["allow", "deny", "ask"] {
                    if let Some(incoming) = configured.member(name) {
                        merge_rules(&entry.rules.member(name), &incoming);
                    }
                }
            }
            return (entry.id, entry.rules.clone());
        }

        let id = self.next_id;
        self.next_id += 1;
        // The baseline's first `acquire` adopts each configured array by
        // reference and creates a fresh `[]` for every missing member. Sharing
        // the per-member arrays (not the whole rule set) keeps grants visible
        // through `SessionConfigOverrides::get` while letting a recreate start
        // with new arrays for members the override never declared.
        let rules = configured
            .map(RuleSetHandle::from_configured)
            .unwrap_or_default();
        self.entries.insert(
            session_key.clone(),
            Entry {
                id,
                rules: rules.clone(),
                retains: 0,
                grant_pinned: false,
            },
        );
        (id, rules)
    }

    fn delete_if_unused(&mut self, session_key: &JsString, id: u64) {
        let remove = self
            .entries
            .get(session_key)
            .is_some_and(|entry| entry.id == id && entry.retains == 0 && !entry.grant_pinned);
        if remove {
            self.entries.remove(session_key);
        }
    }

    fn entry_mut(&mut self, session_key: &JsString, id: u64) -> Option<&mut Entry> {
        self.entries
            .get_mut(session_key)
            .filter(|entry| entry.id == id)
    }
}

fn lock(inner: &Arc<Mutex<Inner>>) -> MutexGuard<'_, Inner> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Live view of one session's `allow` array. The baseline returns the actual
/// `readonly PermissionRule[]`, so a grant that lands while a caller is
/// awaiting is visible when the caller finally reads it. Hold this handle and
/// snapshot at the point of use; the handle stays valid after the map entry is
/// removed, like a released lease's `rules`.
#[derive(Clone)]
pub struct AllowRulesHandle {
    allow: SessionValue,
}

impl AllowRulesHandle {
    pub fn snapshot(&self) -> Vec<PermissionRule> {
        snapshot_array(&self.allow)
    }
    pub fn to_value(&self) -> JsValue {
        self.allow.to_value()
    }
    pub fn value(&self) -> SessionValue {
        self.allow.clone()
    }
    pub fn len(&self) -> usize {
        self.allow.items().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The exact rules object shared by every lease in one generation. Its members
/// can be mutated in place or assigned new arrays. Previously held array views
/// keep their original array when a member is replaced.
#[derive(Clone)]
pub struct RuleSetHandle {
    value: SessionValue,
}

impl Default for RuleSetHandle {
    fn default() -> Self {
        Self::from_configured(&ConfiguredRuleSet::from_shared(SessionValue::object()))
    }
}

impl RuleSetHandle {
    pub fn new(rules: PermissionRuleSet) -> Self {
        Self::from_configured(&ConfiguredRuleSet::new(rules))
    }
    pub fn empty() -> Self {
        Self::default()
    }
    fn from_configured(configured: &ConfiguredRuleSet) -> Self {
        let value = SessionValue::object();
        for name in ["allow", "deny", "ask"] {
            value.set(name, configured.member(name).unwrap_or_else(new_array));
        }
        Self { value }
    }
    pub fn value(&self) -> SessionValue {
        self.value.clone()
    }
    pub fn member(&self, name: &str) -> SessionValue {
        self.value
            .get(name)
            .filter(SessionValue::is_array)
            .expect("rule array")
    }
    /// Production mutation API for all three arrays, including default members.
    /// Callbacks must not re-enter the same array or a registry sharing it.
    pub fn update_member<R>(
        &self,
        name: &str,
        update: impl FnOnce(&mut Vec<SessionValue>) -> R,
    ) -> R {
        self.member(name).update_array(update)
    }
    pub fn snapshot(&self) -> PermissionRuleSet {
        PermissionRuleSet {
            allow: snapshot_array(&self.member("allow")),
            deny: snapshot_array(&self.member("deny")),
            ask: snapshot_array(&self.member("ask")),
        }
    }
    pub fn to_value(&self) -> JsValue {
        self.value.to_value()
    }
}

/// A live configured object. Missing/null/undefined members get independent
/// default arrays in each generation; later member assignments affect future
/// generations, without changing arrays already captured by an existing entry.
#[derive(Clone)]
pub struct ConfiguredRuleSet {
    value: SessionValue,
}

impl ConfiguredRuleSet {
    /// Detached input boundary. Use from_shared for an existing live object.
    pub fn from_permission_rules(value: &JsValue) -> Self {
        Self::from_shared(value.clone().into())
    }
    pub fn from_shared(value: SessionValue) -> Self {
        Self { value }
    }
    pub fn value(&self) -> SessionValue {
        self.value.clone()
    }
    pub fn new(rules: PermissionRuleSet) -> Self {
        Self::from_permission_rules(&rules.to_value())
    }
    pub fn empty() -> Self {
        Self::new(PermissionRuleSet::default())
    }
    pub fn same_object(&self, other: &Self) -> bool {
        self.value.same_object(&other.value)
    }
    pub fn member(&self, name: &str) -> Option<SessionValue> {
        self.value.get(name).filter(SessionValue::is_array)
    }
    pub fn snapshot(&self) -> PermissionRuleSet {
        RuleSetHandle::from_configured(self).snapshot()
    }
    pub fn update_member(&self, name: &str, update: impl FnOnce(&mut Vec<SessionValue>)) -> bool {
        let Some(array) = self.member(name) else {
            return false;
        };
        array.update_array(update);
        true
    }
}

/// `GatewaySessionPermissionRuleSetLease`: one retain on an entry.
pub struct Lease {
    inner: Arc<Mutex<Inner>>,
    session_key: JsString,
    id: u64,
    rules: RuleSetHandle,
    released: AtomicBool,
}

impl Lease {
    pub fn rules(&self) -> RuleSetHandle {
        self.rules.clone()
    }

    /// Idempotent. The final release removes fallback state once no user grant
    /// or overlapping lease keeps the entry alive.
    pub fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut inner = lock(&self.inner);
        let Some(entry) = inner.entry_mut(&self.session_key, self.id) else {
            return;
        };
        entry.retains = entry.retains.saturating_sub(1);
        inner.delete_if_unused(&self.session_key, self.id);
    }
}

/// `GatewaySessionPermissionRuleSetRegistry`. The baseline object is
/// single-threaded; the Rust port shares its state through a mutex because
/// leases outlive a registry borrow and a Gateway handler is `Send + Sync`.
#[derive(Default)]
pub struct GatewaySessionPermissionRuleSetRegistry {
    inner: Arc<Mutex<Inner>>,
}

impl GatewaySessionPermissionRuleSetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(&self) -> usize {
        lock(&self.inner).entries.len()
    }

    pub fn acquire(&self, session_key: &JsString, configured: Option<&ConfiguredRuleSet>) -> Lease {
        let mut inner = lock(&self.inner);
        let (id, rules) = inner.ensure(session_key, configured);
        if let Some(entry) = inner.entry_mut(session_key, id) {
            entry.retains += 1;
        }
        Lease {
            inner: Arc::clone(&self.inner),
            session_key: session_key.clone(),
            id,
            rules,
            released: AtomicBool::new(false),
        }
    }

    /// Grants a rule object with the PermissionRule field types. Passing an
    /// existing SessionValue retains that object's identity; the value builder
    /// PermissionRule creates a new object. This typed port does not emulate
    /// exceptions for JavaScript calls outside the PermissionRule input domain.
    pub fn grant(&self, session_key: &JsString, rule: impl Into<SessionValue>) -> bool {
        let rule = rule.into();
        let mut inner = lock(&self.inner);
        let (id, _) = inner.ensure(session_key, None);
        let Some(entry) = inner.entry_mut(session_key, id) else {
            return false;
        };
        entry.grant_pinned = true;
        entry.rules.update_member("allow", |allow| {
            if allow.iter().any(|existing| same_rule(existing, &rule)) {
                return false;
            }
            allow.push(rule);
            true
        })
    }

    /// The baseline returns its live `readonly PermissionRule[]`; the returned
    /// handle is that view. Hold it and read [`AllowRulesHandle::snapshot`] at
    /// the point of use, not before an await, so a grant that lands while the
    /// caller is awaiting is still observed.
    pub fn allow_rules(&self, session_key: &JsString) -> AllowRulesHandle {
        let allow = lock(&self.inner)
            .entries
            .get(session_key)
            .map(|entry| entry.rules.member("allow"));
        AllowRulesHandle {
            allow: allow.unwrap_or_else(new_array),
        }
    }

    /// Mutates an already-retained session rule set without creating fallback
    /// state. The callback must not re-enter this registry.
    pub fn update_ask_rules(
        &self,
        session_key: &JsString,
        update: impl FnOnce(&mut Vec<SessionValue>),
    ) -> bool {
        let ask = lock(&self.inner)
            .entries
            .get(session_key)
            .map(|entry| entry.rules.member("ask"));
        let Some(ask) = ask else {
            return false;
        };
        ask.update_array(update);
        true
    }

    pub fn close_session(&self, session_key: &JsString) {
        let mut inner = lock(&self.inner);
        let Some(entry) = inner.entries.get_mut(session_key) else {
            return;
        };
        entry.grant_pinned = false;
        let id = entry.id;
        inner.delete_if_unused(session_key, id);
    }

    pub fn dispose(&self) {
        lock(&self.inner).entries.clear();
    }
}

/// `PermissionMode` at this layer is an unvalidated string, matching the
/// baseline registry, which stores whatever the caller passes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PermissionMode(JsString);

impl PermissionMode {
    pub const DEFAULT: &'static str = "default";
    pub const PLAN: &'static str = "plan";
    pub const BYPASS_PERMISSIONS: &'static str = "bypassPermissions";

    pub fn new(value: impl Into<JsString>) -> Self {
        Self(value.into())
    }

    pub fn as_js_string(&self) -> &JsString {
        &self.0
    }

    pub fn is_plan(&self) -> bool {
        self.0.equals_str(Self::PLAN)
    }
}

impl From<&str> for PermissionMode {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

impl From<JsString> for PermissionMode {
    fn from(value: JsString) -> Self {
        Self(value)
    }
}

struct ModeEntry {
    current: PermissionMode,
    base_before_plan: Option<PermissionMode>,
}

/// `GatewaySessionPermissionModeRegistry`: deliberately volatile host-owned
/// mode state. SessionRuntime remains the durable source of truth.
#[derive(Default)]
pub struct GatewaySessionPermissionModeRegistry {
    modes: HashMap<JsString, ModeEntry>,
}

impl GatewaySessionPermissionModeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, session_key: &JsString) -> Option<PermissionMode> {
        self.modes
            .get(session_key)
            .map(|entry| entry.current.clone())
    }

    pub fn get_base(&self, session_key: &JsString) -> Option<PermissionMode> {
        self.modes
            .get(session_key)
            .and_then(|entry| entry.base_before_plan.clone())
    }

    pub fn set(&mut self, session_key: &JsString, mode: PermissionMode) {
        self.modes.insert(
            session_key.clone(),
            ModeEntry {
                current: mode,
                base_before_plan: None,
            },
        );
    }

    pub fn transition(
        &mut self,
        session_key: &JsString,
        mode: PermissionMode,
        fallback_base: Option<PermissionMode>,
    ) {
        if mode.is_plan() {
            let base = self
                .modes
                .get(session_key)
                .and_then(|previous| {
                    previous
                        .base_before_plan
                        .clone()
                        .or_else(|| Some(previous.current.clone()))
                })
                .or(fallback_base);
            self.modes.insert(
                session_key.clone(),
                ModeEntry {
                    current: mode,
                    base_before_plan: base,
                },
            );
            return;
        }
        self.set(session_key, mode);
    }

    pub fn clear(&mut self, session_key: &JsString) {
        self.modes.remove(session_key);
    }

    pub fn dispose(&mut self) {
        self.modes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Barrier, thread};

    fn key(value: &str) -> JsString {
        value.into()
    }

    fn rule(source: &str, behavior: &str, tool: &str) -> PermissionRule {
        PermissionRule::new(source, behavior, tool)
    }

    #[test]
    fn concurrent_acquire_grant_and_close_release_keep_one_entry_and_rule() {
        const WORKERS: usize = 8;
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("shared");
        let barrier = Barrier::new(WORKERS);
        let results = thread::scope(|scope| {
            let handles: Vec<_> = (0..WORKERS)
                .map(|index| {
                    let (registry, session, barrier) = (&registry, &session, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        let lease = registry.acquire(session, None);
                        barrier.wait();
                        // force is deliberately excluded from sameRule.
                        let granted = registry.grant(
                            session,
                            rule("session", "allow", "Read").with_force(index % 2 == 0),
                        );
                        (lease, granted)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("grant worker"))
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|(_, granted)| *granted).count(), 1);
        assert_eq!(registry.size(), 1);
        let expected = registry.allow_rules(&session).snapshot();
        assert_eq!(expected.len(), 1);
        for (lease, _) in &results {
            assert_eq!(lease.rules().snapshot().allow, expected);
        }

        let barrier = Barrier::new(WORKERS + 1);
        thread::scope(|scope| {
            for (lease, _) in &results {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    lease.release();
                    lease.release();
                });
            }
            scope.spawn(|| {
                barrier.wait();
                registry.close_session(&session);
            });
        });
        assert_eq!(registry.size(), 0);
        assert!(registry.allow_rules(&session).is_empty());
    }

    #[test]
    fn concurrent_dispose_and_old_releases_preserve_replacement() {
        const WORKERS: usize = 8;
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("shared");
        let retained = registry.acquire(&session, None);
        let old: Vec<_> = (0..WORKERS)
            .map(|_| registry.acquire(&session, None))
            .collect();
        let read = rule("session", "allow", "Read");
        assert!(registry.grant(&session, read.clone()));
        let barrier = Barrier::new(WORKERS + 1);
        let replacement = thread::scope(|scope| {
            for lease in &old {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    lease.release();
                });
            }
            scope
                .spawn(|| {
                    barrier.wait();
                    registry.dispose();
                    registry.acquire(&session, None)
                })
                .join()
                .expect("dispose worker")
        });
        assert_eq!(registry.size(), 1);
        assert!(replacement.rules().snapshot().allow.is_empty());
        assert_eq!(retained.rules().snapshot().allow, vec![read]);
        // Guarantee a first release from the old generation after replacement,
        // independently of how the racing workers above were scheduled.
        retained.release();
        retained.release();
        assert_eq!(registry.size(), 1);
        replacement.release();
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn final_release_removes_unpinned_entry_and_release_is_idempotent() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let first = registry.acquire(&session, None);
        let second = registry.acquire(&session, None);
        assert_eq!(registry.size(), 1);
        first.release();
        first.release();
        assert_eq!(registry.size(), 1);
        second.release();
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn grant_pins_entry_until_close_session() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        assert!(registry.grant(&session, rule("session", "allow", "Read")));
        assert!(!registry.grant(&session, rule("session", "allow", "Read")));
        let lease = registry.acquire(&session, None);
        lease.release();
        assert_eq!(registry.size(), 1);
        registry.close_session(&session);
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn configured_rules_merge_into_an_existing_entry_without_duplicates() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let first = rule("user", "allow", "Read");
        let duplicate = PermissionRule {
            force: Some(true),
            ..first.clone()
        };
        let extra = PermissionRule::new("project", "deny", "Write");
        assert!(registry.grant(&session, duplicate));
        let configured = PermissionRuleSet {
            allow: vec![first.clone()],
            deny: vec![extra.clone()],
            ask: vec![rule("policy", "ask", "Bash")],
        };
        let handle = ConfiguredRuleSet::new(configured);
        let lease = registry.acquire(&session, Some(&handle));
        let rules = lease.rules().snapshot();
        assert_eq!(rules.allow.len(), 1);
        assert_eq!(rules.deny, vec![extra]);
        assert_eq!(rules.ask.len(), 1);
        assert!(!registry.grant(&session, first));
    }

    #[test]
    fn create_keeps_configured_duplicates_and_configured_order() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let duplicate = rule("user", "allow", "Read");
        let configured = PermissionRuleSet {
            allow: vec![duplicate.clone(), duplicate, rule("user", "allow", "Write")],
            ..PermissionRuleSet::default()
        };
        let handle = ConfiguredRuleSet::new(configured);
        let lease = registry.acquire(&session, Some(&handle));
        let rules = lease.rules().snapshot();
        assert_eq!(rules.allow.len(), 3);
        assert_eq!(rules.allow[0].tool_name, key("Read"));
        assert_eq!(rules.allow[2].tool_name, key("Write"));
    }

    #[test]
    fn configured_handle_stays_shared_with_the_registry_across_acquires() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let handle = ConfiguredRuleSet::empty();
        let first = registry.acquire(&session, Some(&handle));
        let granted = rule("session", "allow", "Read");
        assert!(registry.grant(&session, granted.clone()));
        assert_eq!(handle.snapshot().allow, vec![granted.clone()]);
        assert_eq!(first.rules().snapshot().allow, vec![granted.clone()]);
        first.release();
        registry.close_session(&session);
        assert_eq!(registry.size(), 0);

        // A recreate fed by the same override handle adopts the live array.
        let second = registry.acquire(&session, Some(&handle));
        assert_eq!(second.rules().snapshot().allow, vec![granted.clone()]);
        let extra = rule("user", "allow", "Grep");
        assert!(registry.grant(&session, extra.clone()));
        assert_eq!(handle.snapshot().allow, vec![granted, extra]);
    }

    #[test]
    fn default_members_are_not_shared_between_generations() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        // `permissionRules: {}`: the baseline's `configured?.member ?? []`
        // creates all three arrays per entry, so nothing survives a recreate.
        let configured =
            ConfiguredRuleSet::from_permission_rules(&JsValue::Object(JsObject::new()));
        let first = registry.acquire(&session, Some(&configured));
        assert!(registry.grant(&session, rule("session", "allow", "Read")));
        assert!(registry.update_ask_rules(&session, |ask| {
            ask.push(rule("policy", "ask", "Bash").into())
        }));
        registry.close_session(&session);
        first.release();
        assert_eq!(registry.size(), 0);

        let second = registry.acquire(&session, Some(&configured));
        let rules = second.rules().snapshot();
        assert!(rules.allow.is_empty(), "default allow must be a new array");
        assert!(rules.ask.is_empty(), "default ask must be a new array");
        assert_eq!(configured.snapshot(), PermissionRuleSet::default());
    }

    #[test]
    fn partial_configuration_shares_only_its_declared_arrays() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let mut object = JsObject::new();
        object.insert("allow".into(), JsValue::Array(vec![]));
        let configured = ConfiguredRuleSet::from_permission_rules(&JsValue::Object(object));
        let read = rule("session", "allow", "Read");

        let first = registry.acquire(&session, Some(&configured));
        assert!(registry.grant(&session, read.clone()));
        assert!(registry.update_ask_rules(&session, |ask| {
            ask.push(rule("policy", "ask", "Bash").into())
        }));
        registry.close_session(&session);
        first.release();

        let second = registry.acquire(&session, Some(&configured));
        let rules = second.rules().snapshot();
        assert_eq!(rules.allow, vec![read], "declared allow stays shared");
        assert!(
            rules.ask.is_empty(),
            "undeclared ask is recreated per entry"
        );
        assert_eq!(configured.snapshot().allow.len(), 1);
        assert!(configured.snapshot().ask.is_empty());
    }

    #[test]
    fn one_partial_configured_object_shares_declared_members_across_keys() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let mut object = JsObject::new();
        object.insert("allow".into(), JsValue::Array(vec![]));
        let configured = ConfiguredRuleSet::from_permission_rules(&JsValue::Object(object));
        let a = key("a");
        let b = key("b");
        let first = registry.acquire(&a, Some(&configured));
        let second = registry.acquire(&b, Some(&configured));
        assert!(registry.grant(&a, rule("session", "allow", "Read")));
        assert_eq!(
            second.rules().snapshot().allow,
            vec![rule("session", "allow", "Read")]
        );
        assert!(
            registry.update_ask_rules(&a, |ask| { ask.push(rule("policy", "ask", "Bash").into()) })
        );
        assert!(second.rules().snapshot().ask.is_empty());
        first.release();
        second.release();
        registry.close_session(&a);
        registry.close_session(&b);
    }

    #[test]
    fn allow_rules_handle_stays_live_across_a_later_grant() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let missing = registry.allow_rules(&session);
        assert!(missing.is_empty());
        let lease = registry.acquire(&session, None);
        // Hold the view before the grant, like the Gateway does across awaits.
        let view = registry.allow_rules(&session);
        assert!(view.is_empty());
        let granted = rule("session", "allow", "Read");
        assert!(registry.grant(&session, granted.clone()));
        assert_eq!(view.snapshot(), vec![granted.clone()]);
        lease.release();
        registry.close_session(&session);
        assert_eq!(registry.size(), 0);
        // The view still points at the generation's exact array.
        assert_eq!(view.snapshot(), vec![granted]);
    }

    #[test]
    fn handle_update_propagates_to_leases_and_allow_view() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let handle = ConfiguredRuleSet::empty();
        let lease = registry.acquire(&session, Some(&handle));
        let allow = registry.allow_rules(&session);
        let read = rule("session", "allow", "Read");
        handle.update_member("allow", |allow| allow.push(read.clone().into()));
        assert_eq!(lease.rules().snapshot().allow, vec![read.clone()]);
        assert_eq!(allow.snapshot(), vec![read]);
        handle.update_member("ask", |ask| ask.push(rule("policy", "ask", "Bash").into()));
        assert_eq!(lease.rules().snapshot().ask.len(), 1);
        assert_eq!(handle.snapshot().ask.len(), 1);
        lease.release();
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn handle_updates_reach_every_observer_and_old_release_keeps_replacement() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let handle = ConfiguredRuleSet::empty();
        let first = registry.acquire(&session, Some(&handle));
        let second = registry.acquire(&session, Some(&handle));
        assert!(registry.update_ask_rules(&session, |ask| {
            ask.push(rule("policy", "ask", "Bash").into())
        }));
        assert_eq!(first.rules().snapshot().ask.len(), 1);
        assert_eq!(second.rules().snapshot().ask.len(), 1);
        assert_eq!(handle.snapshot().ask.len(), 1);

        // Recreate after dispose; releasing the old generation must not delete
        // the replacement entry (the baseline compares `entries.get(key)`).
        registry.dispose();
        let replacement = registry.acquire(&session, Some(&handle));
        first.release();
        assert_eq!(registry.size(), 1);
        second.release();
        assert_eq!(registry.size(), 1);
        replacement.release();
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn update_ask_rules_requires_an_existing_entry() {
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let session = key("s");
        let mut called = false;
        assert!(!registry.update_ask_rules(&session, |_| called = true));
        assert!(!called);
        assert_eq!(registry.size(), 0);
        registry.grant(&session, rule("session", "allow", "Read"));
        assert!(registry.update_ask_rules(&session, |rules| {
            rules.push(rule("policy", "ask", "Bash").into())
        }));
        assert_eq!(registry.size(), 1);
    }

    #[test]
    fn plan_transition_preserves_the_pre_plan_base_in_order() {
        let session = key("s");
        let mut registry = GatewaySessionPermissionModeRegistry::new();
        assert_eq!(registry.get(&session), None);
        registry.set(&session, "plan".into());
        assert_eq!(registry.get_base(&session), None);
        registry.transition(&session, "bypassPermissions".into(), None);
        assert_eq!(registry.get_base(&session), None);
        registry.transition(&session, "plan".into(), Some("default".into()));
        assert_eq!(
            registry.get_base(&session),
            Some(PermissionMode::from("bypassPermissions"))
        );
        registry.transition(&session, "plan".into(), Some("default".into()));
        assert_eq!(
            registry.get_base(&session),
            Some(PermissionMode::from("bypassPermissions"))
        );
        let other = key("other");
        registry.transition(&other, "plan".into(), Some("bypassPermissions".into()));
        assert_eq!(
            registry.get_base(&other),
            Some(PermissionMode::from("bypassPermissions"))
        );
        registry.set(&other, "default".into());
        assert_eq!(registry.get_base(&other), None);
    }
}
