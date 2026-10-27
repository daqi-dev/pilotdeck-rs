//! The registry's single `sdkSessionProjects` map
//! (`ProjectRuntimeRegistry.ts:254`): session key → project root, in JS
//! `Map` insertion order. One instance is shared by the session-config slice
//! (`setSdkSessionConfig`) and the SDK MCP slice (`setMcpServersForSdk`,
//! `clearSdkSessionState`) so the two cannot drift apart (#347③); the
//! composition root injects the same [`std::sync::Arc`] into both. The MCP
//! slice holds it as [`SdkSessionProjectsTable`] so the shared-table writes
//! sit behind the commit's critical section (#347 fix-r1).

use crate::runtime::cleanup_task::lock;
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::{Arc, Mutex};

/// JS `Map` key equality (`SameValueZero` over the value kinds the protocol
/// models): `undefined`/`null`/bool/string compare literally, numbers by
/// value with `NaN` equal to itself, and no cross-kind match.
pub(crate) fn same_key(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => a == b || (a.is_nan() && b.is_nan()),
        (JsValue::String(a), JsValue::String(b)) => a == b,
        _ => false,
    }
}

/// The table face the two registry slices use: `set`/`delete` are the writes
/// that must share the SDK MCP commit's critical section (#347 fix-r1), so
/// the MCP config holds it as a trait object and tests interpose a gated
/// implementation over the real table.
pub trait SdkSessionProjectsTable: Send + Sync {
    fn project(&self, session_key: &JsValue) -> Option<JsString>;
    fn set(&self, session_key: &JsValue, project_root: JsString);
    fn delete(&self, session_key: &JsValue);
    fn entries(&self) -> Vec<(JsValue, JsString)>;
}

/// `sdkSessionProjects`: session key → project root (`string` in TS).
#[derive(Default)]
pub struct SdkSessionProjects {
    entries: Mutex<Vec<(JsValue, JsString)>>,
}

impl SdkSessionProjects {
    /// A fresh shared table, as `new Map()` in the registry constructor.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl SdkSessionProjectsTable for SdkSessionProjects {
    /// `map.get(key)`; the first match in insertion order.
    fn project(&self, session_key: &JsValue) -> Option<JsString> {
        lock(&self.entries)
            .iter()
            .find(|(key, _)| same_key(key, session_key))
            .map(|(_, root)| root.clone())
    }

    /// `map.set(key, root)`: replaces an existing key in place, keeping its
    /// insertion position, or appends.
    fn set(&self, session_key: &JsValue, project_root: JsString) {
        let mut entries = lock(&self.entries);
        match entries
            .iter_mut()
            .find(|(key, _)| same_key(key, session_key))
        {
            Some((_, root)) => *root = project_root,
            None => entries.push((session_key.clone(), project_root)),
        }
    }

    /// `map.delete(key)`.
    fn delete(&self, session_key: &JsValue) {
        lock(&self.entries).retain(|(key, _)| !same_key(key, session_key));
    }

    /// Insertion-ordered snapshot (fixture replay and the state projection).
    fn entries(&self) -> Vec<(JsValue, JsString)> {
        lock(&self.entries).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(key: &str) -> JsValue {
        JsValue::String(key.into())
    }

    #[test]
    fn set_replaces_in_place_and_keeps_insertion_order() {
        let table = SdkSessionProjects::default();
        table.set(&value("a"), "/one".into());
        table.set(&value("b"), "/two".into());
        table.set(&value("a"), "/three".into());
        let names: Vec<String> = table
            .entries()
            .iter()
            .map(|(key, root)| {
                format!(
                    "{}={}",
                    String::from_utf16_lossy(key.as_string().unwrap().units()),
                    String::from_utf16_lossy(root.units())
                )
            })
            .collect();
        assert_eq!(
            names,
            vec!["a=/three".to_owned(), "b=/two".to_owned()],
            "#347 set replaces an existing key in place, like the TS Map"
        );
    }

    #[test]
    fn delete_uses_map_key_semantics() {
        let table = SdkSessionProjects::default();
        table.set(&JsValue::Undefined, "/u".into());
        table.set(&JsValue::Number(f64::NAN), "/nan".into());
        table.set(&value("a"), "/a".into());
        table.delete(&JsValue::Undefined);
        table.delete(&JsValue::Number(f64::NAN));
        assert_eq!(
            table.project(&value("a")),
            Some("/a".into()),
            "#347 a matching key deletes the entry"
        );
        assert_eq!(
            table.project(&JsValue::Number(f64::NAN)),
            None,
            "#347 NaN keys delete by SameValueZero equality"
        );
        table.delete(&JsValue::Number(1.0));
        assert_eq!(
            table.project(&value("a")),
            Some("/a".into()),
            "#347 a non-matching key must not delete the entry"
        );
    }
}
