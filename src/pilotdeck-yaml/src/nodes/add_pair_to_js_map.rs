// Port of yaml@2.9.0 dist/nodes/addPairToJSMap.js (ISC); parse/conversion operations.
use super::to_js::{number_string, same_value_zero};
use crate::nodes::to_js::Context;
use crate::{
    nodes::{NodeId, NodeKind, Pair, ScalarValue},
    string::concat,
    value::{Container, ConversionError, Value},
};
use pilotdeck_protocol::JsString;
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
        match &mut self.nodes[index] {
            Container::Object(entries) => {
                let Value::String(key) = key else {
                    unreachable!()
                };
                if let Some((_, v)) = entries.iter_mut().find(|(k, _)| k == key.as_ref()) {
                    if !only_missing {
                        *v = value;
                    }
                } else {
                    entries.push((key.as_ref().clone(), value));
                }
            }
            Container::Map(entries) => {
                if let Some((_, v)) = entries.iter_mut().find(|(k, _)| same_value_zero(k, &key)) {
                    if !only_missing {
                        *v = value;
                    }
                } else {
                    entries.push((key, value));
                }
            }
            Container::Set(entries) => {
                if !entries.iter().any(|k| same_value_zero(k, &key)) {
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
                &self.anchors,
                crate::stack::CONVERSION_DEPTH.saturating_sub(self.depth + self.merge_stack.len()),
            )
        } else {
            Ok(self.js_string(value))
        }
    }
}
