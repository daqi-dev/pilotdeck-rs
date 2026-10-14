// Port of yaml@2.9.0 dist/nodes/addPairToJSMap.js (ISC); parse/conversion operations.
use super::{
    key::Key,
    to_js::{number_string, same_value_zero},
};
use crate::nodes::to_js::Context;
use crate::{
    nodes::{NodeId, NodeKind, Pair, ScalarValue},
    string::concat,
    value::{Container, ConversionError, Value},
};
use pilotdeck_protocol::JsString;
use std::collections::{HashMap, hash_map::RandomState};
use std::hash::BuildHasher;

/// Construction-only index. Keys stay in the ordered container; hash collisions
/// are resolved against those original keys, never treated as equal by hash alone.
#[derive(Default)]
pub(crate) struct KeyIndex {
    hashes: RandomState,
    slots: HashMap<u64, usize>,
    collisions: HashMap<u64, Vec<usize>>,
}
impl KeyIndex {
    fn new(container: &Container) -> Self {
        let mut index = Self::default();
        match container {
            Container::Object(entries) => {
                for (slot, (key, _)) in entries.iter().enumerate() {
                    index.add_hash(index.hashes.hash_one(key), slot);
                }
            }
            Container::Map(entries) => {
                for (slot, (key, _)) in entries.iter().enumerate() {
                    index.add_hash(index.hashes.hash_one(Key::from(key)), slot);
                }
            }
            Container::Set(entries) => {
                for (slot, key) in entries.iter().enumerate() {
                    index.add_hash(index.hashes.hash_one(Key::from(key)), slot);
                }
            }
            _ => unreachable!(),
        }
        index
    }
    fn hash(&self, container: &Container, key: &Value) -> u64 {
        if let Container::Object(_) = container {
            let Value::String(key) = key else {
                unreachable!()
            };
            self.hashes.hash_one(key.as_ref())
        } else {
            self.hashes.hash_one(Key::from(key))
        }
    }
    fn add_hash(&mut self, hash: u64, slot: usize) {
        match self.slots.entry(hash) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(slot);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                self.collisions.entry(hash).or_default().push(slot);
            }
        }
    }
    fn find_hash(&self, hash: u64, container: &Container, key: &Value) -> Option<usize> {
        let first = *self.slots.get(&hash)?;
        std::iter::once(first)
            .chain(self.collisions.get(&hash).into_iter().flatten().copied())
            .find(|&slot| match container {
                Container::Object(entries) => {
                    matches!(key, Value::String(key) if entries[slot].0 == **key)
                }
                Container::Map(entries) => same_value_zero(&entries[slot].0, key),
                Container::Set(entries) => same_value_zero(&entries[slot], key),
                _ => unreachable!(),
            })
    }
    fn find(&self, container: &Container, key: &Value) -> Option<usize> {
        self.find_hash(self.hash(container, key), container, key)
    }
}
impl Context<'_> {
    pub(crate) fn add_pair(&mut self, index: usize, pair: &Pair) -> Result<(), ConversionError> {
        let key_node = &self.doc.nodes[pair.key];
        let merge = matches!(key_node.kind, NodeKind::Scalar(ScalarValue::Merge))
            || (self.doc.schema.yaml11
                && key_node.style == "PLAIN"
                && matches!(&key_node.kind,NodeKind::Scalar(ScalarValue::String(s))if s.equals_str("<<")));
        if merge {
            return self.add_merge(index, pair.value);
        }
        let key = self.convert(Some(pair.key))?;
        if matches!(self.nodes[index], Container::Set(_)) {
            self.insert(index, key, Value::Null, false);
            return Ok(());
        }
        let key = if matches!(self.nodes[index], Container::Object(_)) {
            Value::String(std::sync::Arc::new(self.stringify_key(pair.key, &key)?))
        } else {
            key
        };
        let value = self.convert(pair.value)?;
        self.insert(index, key, value, false);
        Ok(())
    }
    pub(crate) fn has_key(&self, index: usize, key: &Value) -> bool {
        if let Some(keys) = self.key_indices.get(&index) {
            return keys.find(&self.nodes[index], key).is_some();
        }
        match &self.nodes[index] {
            Container::Map(e) => e.iter().any(|(k, _)| same_value_zero(k, key)),
            Container::Set(e) => e.iter().any(|k| same_value_zero(k, key)),
            Container::Object(e) => {
                matches!(key,Value::String(k)if e.iter().any(|(a,_)|a==k.as_ref()))
            }
            _ => false,
        }
    }
    pub(crate) fn insert(&mut self, index: usize, key: Value, value: Value, only_missing: bool) {
        // JS Map/Set canonicalize a -0 key to +0 (the value stays unchanged).
        let key = if matches!(key,Value::Number(n) if n==0.0) {
            Value::Number(0.0)
        } else {
            key
        };
        // Tiny tables keep the cheap scan; after 16 entries all subsequent
        // lookups use a hash index, while the Vec preserves insertion order.
        let len = match &self.nodes[index] {
            Container::Object(e) => e.len(),
            Container::Map(e) => e.len(),
            Container::Set(e) => e.len(),
            _ => unreachable!(),
        };
        if len >= 16 && !self.key_indices.contains_key(&index) {
            self.key_indices
                .insert(index, KeyIndex::new(&self.nodes[index]));
        }
        let indexed = self.key_indices.get_mut(&index).map(|keys| {
            let hash = keys.hash(&self.nodes[index], &key);
            let slot = keys.find_hash(hash, &self.nodes[index], &key);
            if slot.is_none() {
                keys.add_hash(hash, len);
            }
            slot
        });
        match &mut self.nodes[index] {
            Container::Object(entries) => {
                let Value::String(key) = key else {
                    unreachable!()
                };
                if let Some(slot) =
                    indexed.unwrap_or_else(|| entries.iter().position(|(k, _)| k == key.as_ref()))
                {
                    let v = &mut entries[slot].1;
                    if !only_missing {
                        *v = value;
                    }
                } else {
                    entries.push((key.as_ref().clone(), value));
                }
            }
            Container::Map(entries) => {
                if let Some(slot) = indexed
                    .unwrap_or_else(|| entries.iter().position(|(k, _)| same_value_zero(k, &key)))
                {
                    let v = &mut entries[slot].1;
                    if !only_missing {
                        *v = value;
                    }
                } else {
                    entries.push((key, value));
                }
            }
            Container::Set(entries) => {
                if indexed
                    .unwrap_or_else(|| entries.iter().position(|k| same_value_zero(k, &key)))
                    .is_none()
                {
                    entries.push(key);
                }
            }
            _ => unreachable!(),
        }
    }
    pub(crate) fn js_string(&self, v: &Value) -> JsString {
        enum Part<'a> {
            Value(&'a Value),
            End(usize),
            Comma,
        }
        let mut pending = vec![Part::Value(v)];
        let mut active = vec![false; self.nodes.len()];
        let mut output = Vec::new();
        while let Some(part) = pending.pop() {
            let scalar = match part {
                Part::End(id) => {
                    active[id] = false;
                    continue;
                }
                Part::Comma => {
                    output.push(44);
                    continue;
                }
                Part::Value(v) => match v {
                    Value::Null => "null".into(),
                    Value::Bool(b) => if *b { "true" } else { "false" }.into(),
                    Value::Number(n) => number_string(*n),
                    Value::String(s) => s.as_ref().clone(),
                    Value::Symbol { description: s, .. } => {
                        concat(&[&"Symbol(".into(), s, &")".into()])
                    }
                    Value::Ref(id) => match &self.nodes[*id] {
                        Container::Array(items) => {
                            if active[*id] {
                                continue;
                            }
                            active[*id] = true;
                            pending.push(Part::End(*id));
                            for (i, v) in items.iter().enumerate().rev() {
                                if !matches!(v, Value::Null) {
                                    pending.push(Part::Value(v));
                                }
                                if i > 0 {
                                    pending.push(Part::Comma);
                                }
                            }
                            continue;
                        }
                        Container::Map(_) => "[object Map]".into(),
                        Container::Set(_) => "[object Set]".into(),
                        _ => "[object Object]".into(),
                    },
                },
            };
            output.extend_from_slice(scalar.units());
        }
        JsString::from_utf16(output)
    }
    pub(crate) fn stringify_key(
        &self,
        key: NodeId,
        value: &Value,
    ) -> Result<JsString, ConversionError> {
        if matches!(value, Value::Null) {
            Ok(JsString::default())
        } else if matches!(value, Value::Ref(_)) {
            super::stringify_key::stringify_key(
                self.doc,
                key,
                &self.anchor_names,
                crate::stack::CONVERSION_DEPTH.saturating_sub(self.depth + self.merge_stack.len()),
            )
        } else {
            Ok(self.js_string(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_collisions_compare_original_keys_in_all_container_types() {
        let a = Value::String(std::sync::Arc::new("a".into()));
        let b = Value::String(std::sync::Arc::new("b".into()));
        let missing = Value::String(std::sync::Arc::new("missing".into()));
        for container in [
            Container::Object(vec![("a".into(), Value::Null), ("b".into(), Value::Null)]),
            Container::Map(vec![(a.clone(), Value::Null), (b.clone(), Value::Null)]),
            Container::Set(vec![a.clone(), b.clone()]),
        ] {
            let mut index = KeyIndex::default();
            index.add_hash(7, 0);
            index.add_hash(7, 1);
            assert_eq!(index.find_hash(7, &container, &a), Some(0));
            assert_eq!(index.find_hash(7, &container, &b), Some(1));
            assert_eq!(index.find_hash(7, &container, &missing), None);
        }
    }

    #[test]
    fn indices_are_released_after_merge_and_collection_construction() {
        let entries = (0..40)
            .map(|i| format!("k{i}: {i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let doc = crate::parse_document(
            &format!(
                "%YAML 1.1\n---\n[a: &a {{{entries}}}, {{<<: *a}}, !!set {{{}}}, !!omap [{}]]",
                (0..40)
                    .map(|i| format!("s{i}: null"))
                    .collect::<Vec<_>>()
                    .join(", "),
                (0..40)
                    .map(|i| format!("{{m{i}: {i}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into(),
            Default::default(),
        );
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
        let mut ctx = Context {
            doc: &doc,
            nodes: vec![],
            anchors: Default::default(),
            alias_sources: Default::default(),
            anchor_names: Default::default(),
            key_indices: Default::default(),
            merge_stack: vec![],
            depth: 0,
        };
        ctx.visit_order(doc.contents.unwrap()).unwrap();
        ctx.convert(doc.contents).unwrap();
        assert!(
            ctx.key_indices.is_empty(),
            "completed tables retain no index"
        );
        assert!(ctx.merge_stack.is_empty());
        assert_eq!(
            ctx.nodes
                .iter()
                .map(|node| match node {
                    Container::Map(entries) => entries.len(),
                    _ => 0,
                })
                .sum::<usize>(),
            40,
            "only the real omap retains Map entries"
        );
    }

    #[test]
    fn ordered_indices_preserve_same_value_zero_identity_and_merge_precedence() {
        let doc = crate::parse_document(&"null".into(), Default::default());
        let mut ctx = Context {
            doc: &doc,
            nodes: vec![
                Container::Map(vec![]),
                Container::Set(vec![]),
                Container::Object(vec![]),
            ],
            anchors: Default::default(),
            alias_sources: Default::default(),
            anchor_names: Default::default(),
            key_indices: Default::default(),
            merge_stack: vec![],
            depth: 0,
        };
        for i in 0..32 {
            ctx.insert(0, Value::Number(i as f64), Value::Number(i as f64), false);
            ctx.insert(1, Value::Number(i as f64), Value::Null, false);
            ctx.insert(
                2,
                Value::String(std::sync::Arc::new(format!("k{i}").into())),
                Value::Number(i as f64),
                false,
            );
        }
        ctx.insert(0, Value::Number(-0.0), Value::Bool(true), false);
        ctx.insert(0, Value::Number(0.0), Value::Bool(false), true);
        for n in [f64::NAN, f64::from_bits(0x7ff8000000000001)] {
            ctx.insert(0, Value::Number(n), Value::Bool(true), false);
            ctx.insert(1, Value::Number(n), Value::Null, false);
        }
        ctx.insert(1, Value::Number(-0.0), Value::Null, false);
        for id in [100, 101, 100] {
            ctx.insert(
                0,
                Value::Symbol {
                    id,
                    description: "same".into(),
                },
                Value::Null,
                false,
            );
            ctx.insert(1, Value::Ref(id), Value::Null, false);
        }
        let key = Value::String(std::sync::Arc::new("k0".into()));
        ctx.insert(2, key.clone(), Value::Bool(true), false);
        ctx.insert(2, key, Value::Bool(false), true);
        let Container::Map(entries) = &ctx.nodes[0] else {
            panic!()
        };
        assert_eq!(entries.len(), 35);
        assert!(matches!(entries[0], (Value::Number(n), Value::Bool(true)) if n.to_bits() == 0));
        assert!(matches!(entries[32].0, Value::Number(n) if n.is_nan()));
        assert!(ctx.has_key(0, &Value::Number(f64::NAN)));
        let Container::Set(entries) = &ctx.nodes[1] else {
            panic!()
        };
        assert_eq!(entries.len(), 35);
        let Container::Object(entries) = &ctx.nodes[2] else {
            panic!()
        };
        assert_eq!(entries.len(), 32);
        assert_eq!(entries[0], ("k0".into(), Value::Bool(true)));
        assert_eq!(entries[31].0, JsString::from("k31"));
        assert_eq!(ctx.key_indices.len(), 3);
    }
}
