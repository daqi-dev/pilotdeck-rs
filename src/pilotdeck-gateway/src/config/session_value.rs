//! Live, shallow-copyable session configuration values. `JsValue` is only the
//! serialization boundary: convert once, then clone handles to retain identity.
//! Session configuration is an acyclic object/array graph, with scalar leaves.
//! No process-global table owns these values; the last handle frees each node.

use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Debug)]
pub struct SessionValue(Arc<Mutex<Node>>);

#[derive(Clone, Debug)]
enum Node {
    Scalar(JsValue),
    Object(Vec<(JsString, SessionValue)>),
    Array(Vec<SessionValue>),
}

impl SessionValue {
    fn lock(&self) -> MutexGuard<'_, Node> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn object() -> Self {
        Self(Arc::new(Mutex::new(Node::Object(vec![]))))
    }

    pub fn array(items: Vec<Self>) -> Self {
        Self(Arc::new(Mutex::new(Node::Array(items))))
    }

    pub fn is_object(&self) -> bool {
        matches!(*self.lock(), Node::Object(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(*self.lock(), Node::Array(_))
    }

    /// Reference equality for objects/arrays; scalar values have no identity.
    pub fn same_object(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) && (self.is_object() || self.is_array())
    }

    pub fn get(&self, name: &str) -> Option<Self> {
        match &*self.lock() {
            Node::Object(entries) => entries
                .iter()
                .find(|(k, _)| k.equals_str(name))
                .map(|(_, v)| v.clone()),
            Node::Array(items) => name
                .parse::<usize>()
                .ok()
                .and_then(|i| items.get(i))
                .cloned(),
            _ => None,
        }
    }

    /// Sets an own object property. Values must not introduce a reference cycle.
    pub fn set(&self, name: impl Into<JsString>, value: impl Into<Self>) {
        let name = name.into();
        let value = value.into();
        let mut node = self.lock();
        let Node::Object(entries) = &mut *node else {
            panic!("expected session object")
        };
        if let Some((_, old)) = entries.iter_mut().find(|(key, _)| *key == name) {
            *old = value;
        } else {
            entries.push((name, value));
        }
    }

    /// Deletes an own object property, keeping every remaining nested handle.
    /// Callers should mutate a [`Self::spread`] so the store's top level is
    /// replaced while the nested objects stay shared.
    pub fn remove(&self, name: &str) -> Option<Self> {
        let mut node = self.lock();
        let Node::Object(entries) = &mut *node else {
            panic!("expected session object")
        };
        entries
            .iter()
            .position(|(key, _)| key.equals_str(name))
            .map(|index| entries.remove(index).1)
    }

    pub fn items(&self) -> Vec<Self> {
        let Node::Array(items) = &*self.lock() else {
            panic!("expected session array")
        };
        items.clone()
    }

    /// Mutates the actual array, including remove/replace/clear operations.
    /// The callback must not re-enter this array, call a registry sharing it,
    /// or introduce a reference cycle.
    pub fn update_array<R>(&self, update: impl FnOnce(&mut Vec<Self>) -> R) -> R {
        let mut node = self.lock();
        let Node::Array(items) = &mut *node else {
            panic!("expected session array")
        };
        update(items)
    }

    /// JavaScript `{ ...value }`: a new top-level object, shared nested nodes.
    pub fn spread(&self) -> Self {
        let node = self.lock().clone();
        let entries = match node {
            Node::Object(entries) => entries,
            Node::Array(items) => items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (i.to_string().into(), v))
                .collect(),
            Node::Scalar(JsValue::String(ref s)) => s
                .units()
                .iter()
                .enumerate()
                .map(|(i, u)| {
                    (
                        i.to_string().into(),
                        JsValue::String(JsString::from_utf16(vec![*u])).into(),
                    )
                })
                .collect(),
            Node::Scalar(_) => vec![],
        };
        Self(Arc::new(Mutex::new(Node::Object(entries))))
    }

    /// Detached boundary snapshot; never pass this back when retaining identity.
    /// Only one node is locked at a time, even when multiple fields alias it.
    /// Concurrent mutations can interleave; this is not a graph-atomic snapshot.
    pub fn to_value(&self) -> JsValue {
        let node = self.lock().clone();
        match node {
            Node::Scalar(value) => value,
            Node::Array(items) => JsValue::Array(items.iter().map(Self::to_value).collect()),
            Node::Object(entries) => {
                let mut object = JsObject::new();
                for (name, value) in entries {
                    object.insert(name, value.to_value());
                }
                JsValue::Object(object)
            }
        }
    }
}

impl From<JsValue> for SessionValue {
    fn from(value: JsValue) -> Self {
        let node = match &value {
            JsValue::Object(o) => Node::Object(
                o.entries()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone().into()))
                    .collect(),
            ),
            JsValue::Array(items) => Node::Array(items.iter().cloned().map(Self::from).collect()),
            scalar => Node::Scalar(scalar.clone()),
        };
        Self(Arc::new(Mutex::new(node)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        permission::GatewaySessionPermissionRuleSetRegistry,
        session_overrides::SessionConfigOverrides,
    };

    #[test]
    fn last_owner_releases_nested_nodes() {
        let value = SessionValue::array(vec![]);
        let weak = Arc::downgrade(&value.0);
        let object = SessionValue::object();
        object.set("array", value.clone());
        let view = object.spread();
        drop(value);
        drop(object);
        assert!(weak.upgrade().is_some());
        drop(view);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn override_churn_releases_every_historical_array() {
        let oracle = crate::test_baseline::oracle(include_str!(
            "../../tests/fixtures/session-overrides/oracle.json"
        ));
        for operation in ["replace", "delete", "deletePrefix", "clear", "drop"] {
            let mut store = SessionConfigOverrides::new();
            let mut weak = vec![];
            for _ in 0..100 {
                let array = SessionValue::array(vec![]);
                weak.push(Arc::downgrade(&array.0));
                let rules = SessionValue::object();
                rules.set("allow", array);
                let input = SessionValue::object();
                input.set("permissionRules", rules);
                store.set(&"s".into(), &input);
                match operation {
                    "delete" => store.delete(&"s".into()),
                    "deletePrefix" => store.delete_prefix(&"s".into()),
                    "clear" => store.clear(),
                    _ => {}
                }
            }
            if operation == "replace" {
                store.set(&"s".into(), &SessionValue::object());
            }
            // In every non-drop case keep the store alive while checking: an
            // instance-owned history table would also fail this regression.
            if operation == "drop" {
                drop(store);
            }
            let alive = weak.iter().filter(|w| w.upgrade().is_some()).count();
            let expected = oracle
                .get("lifetime")
                .unwrap()
                .get(operation)
                .unwrap()
                .as_number()
                .unwrap();
            assert_eq!(alive as f64, expected, "{operation}");
        }
    }

    #[test]
    fn views_and_leases_own_only_the_graph_they_still_retain() {
        let mut store = SessionConfigOverrides::new();
        let input = SessionValue::object();
        let rules = SessionValue::object();
        let array = SessionValue::array(vec![]);
        let weak = Arc::downgrade(&array.0);
        rules.set("allow", array);
        input.set("permissionRules", rules);
        store.set(&"s".into(), &input);
        drop(input);
        let view = store.get(&"s".into()).unwrap();
        let registry = GatewaySessionPermissionRuleSetRegistry::new();
        let lease = registry.acquire(&"s".into(), store.permission_rules(&"s".into()).as_ref());
        let allow = registry.allow_rules(&"s".into());
        store.clear();
        drop(store);
        registry.dispose();
        lease.release();
        drop(registry);
        assert!(weak.upgrade().is_some());
        drop(view);
        drop(lease);
        assert!(
            weak.upgrade().is_some(),
            "held allow keeps the original array"
        );
        drop(allow);
        assert!(weak.upgrade().is_none(), "no global roots remain");
    }
}
