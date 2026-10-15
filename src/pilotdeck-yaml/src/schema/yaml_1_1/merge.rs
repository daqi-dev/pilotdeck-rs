// Port of yaml@2.9.0 dist/schema/yaml-1.1/merge.js (ISC); parse/conversion operations.
use crate::nodes::to_js::Context;
use crate::{
    nodes::{NodeId, NodeKind},
    value::{Container, ConversionError, Value},
};
impl Context<'_> {
    pub(crate) fn add_merge(
        &mut self,
        index: usize,
        value: Option<NodeId>,
    ) -> Result<(), ConversionError> {
        let source = value.and_then(|v| self.resolve_alias(v));
        if let Some(source) = source
            && let NodeKind::Seq(items) = self.doc.nodes[source].kind.clone()
        {
            for item in items {
                self.merge_value(index, Some(item))?;
            }
            return Ok(());
        }
        self.merge_value(index, source)
    }
    pub(crate) fn merge_value(
        &mut self,
        index: usize,
        value: Option<NodeId>,
    ) -> Result<(), ConversionError> {
        let source = value.and_then(|v| self.resolve_alias(v));
        let Some(source) = source else {
            return Err(ConversionError::new(
                "Error",
                "Merge sources must be maps or map aliases",
            ));
        };
        let pairs = match &self.doc.nodes[source].kind {
            NodeKind::Map(p) | NodeKind::Set(p) => p.clone(),
            _ => {
                return Err(ConversionError::new(
                    "Error",
                    "Merge sources must be maps or map aliases",
                ));
            }
        };
        // JS recurses until RangeError for a cyclic merge. Detect that cycle
        // explicitly because Rust cannot recover from native stack overflow.
        if self.merge_stack.contains(&source)
            || self.depth + self.merge_stack.len() >= crate::stack::CONVERSION_DEPTH
        {
            return Err(ConversionError::new(
                "RangeError",
                "Maximum call stack size exceeded",
            ));
        }
        self.merge_stack.push(source);
        // YAMLSet.toJSON ignores the requested Map constructor. JS then
        // destructures each Set element as an iterable [key, value].
        let is_set = matches!(self.doc.nodes[source].kind, NodeKind::Set(_));
        let map = self.allocate(if is_set {
            Container::Set(Vec::new())
        } else {
            Container::Map(Vec::new())
        });
        let Value::Ref(map_id) = map else {
            unreachable!()
        };
        for pair in &pairs {
            self.add_pair(map_id, pair)?;
        }
        self.key_indices.remove(&map_id);
        // This collection is a private merge accumulator, never an anchored value.
        // Move its entries out so repeated merges also release its storage.
        let entries = match std::mem::replace(&mut self.nodes[map_id], Container::Map(Vec::new())) {
            Container::Map(entries) => entries,
            Container::Set(items) => items
                .iter()
                .map(|item| self.merge_set_entry(item))
                .collect::<Result<Vec<_>, _>>()?,
            _ => unreachable!(),
        };
        for (key, value) in entries {
            let key = if matches!(self.nodes[index], Container::Object(_)) {
                Value::String(std::sync::Arc::new(self.js_string(&key)))
            } else {
                key
            };
            self.insert(index, key, value, true);
        }
        self.merge_stack.pop();
        Ok(())
    }

    fn merge_set_entry(&mut self, item: &Value) -> Result<(Value, Value), ConversionError> {
        let values = match item {
            Value::String(text) => {
                // JS string iteration keeps surrogate pairs together.
                let mut units = text.units().iter().copied().peekable();
                let mut values = Vec::new();
                while values.len() < 2 {
                    let Some(first) = units.next() else { break };
                    let mut point = vec![first];
                    if (0xd800..=0xdbff).contains(&first)
                        && units.peek().is_some_and(|u| (0xdc00..=0xdfff).contains(u))
                    {
                        point.push(units.next().unwrap());
                    }
                    values.push(Value::String(std::sync::Arc::new(
                        pilotdeck_protocol::JsString::from_utf16(point),
                    )));
                }
                values
            }
            Value::Ref(id) => match &self.nodes[*id] {
                Container::Array(items) | Container::Set(items) => {
                    items.iter().take(2).cloned().collect()
                }
                Container::Map(entries) => {
                    let entries: Vec<_> = entries.iter().take(2).cloned().collect();
                    entries
                        .into_iter()
                        .map(|(key, value)| self.allocate(Container::Array(vec![key, value])))
                        .collect()
                }
                _ => return Err(ConversionError::new("TypeError", ".for is not iterable")),
            },
            _ => return Err(ConversionError::new("TypeError", ".for is not iterable")),
        };
        Ok((
            values.first().cloned().unwrap_or(Value::Undefined),
            values.get(1).cloned().unwrap_or(Value::Undefined),
        ))
    }
}
