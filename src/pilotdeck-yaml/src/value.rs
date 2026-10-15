//! JS value graph. References preserve alias identity and cycles without Rc cycles.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(std::sync::Arc<JsString>),
    Symbol { id: usize, description: JsString },
    Ref(usize),
}
#[derive(Clone, Debug, PartialEq)]
pub enum Container {
    Object(Vec<(JsString, Value)>),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
}
#[derive(Clone, Debug, PartialEq)]
pub struct ValueGraph {
    pub root: Value,
    pub nodes: Vec<Container>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversionError {
    pub name: &'static str,
    pub message: JsString,
}
impl ConversionError {
    pub(crate) fn new(name: &'static str, msg: impl Into<JsString>) -> Self {
        Self {
            name,
            message: msg.into(),
        }
    }
}
impl ValueGraph {
    /// Converted collection keys become strings in JS objects. Their temporary
    /// containers are then garbage; retaining them made successive documents
    /// accumulate every nested key's intermediate conversion graph.
    pub(crate) fn retain_reachable(mut self) -> Self {
        let mut reachable = vec![false; self.nodes.len()];
        let mut pending = Vec::new();
        fn enqueue(value: &Value, pending: &mut Vec<usize>) {
            if let Value::Ref(id) = value {
                pending.push(*id);
            }
        }
        enqueue(&self.root, &mut pending);
        while let Some(id) = pending.pop() {
            if std::mem::replace(&mut reachable[id], true) {
                continue;
            }
            match &self.nodes[id] {
                Container::Object(entries) => {
                    for (_, value) in entries {
                        enqueue(value, &mut pending);
                    }
                }
                Container::Array(items) | Container::Set(items) => {
                    for value in items {
                        enqueue(value, &mut pending);
                    }
                }
                Container::Map(entries) => {
                    for (key, value) in entries {
                        enqueue(key, &mut pending);
                        enqueue(value, &mut pending);
                    }
                }
            }
        }
        let count = reachable.iter().filter(|keep| **keep).count();
        if count == self.nodes.len() {
            return self;
        }
        let mut indices = vec![0; self.nodes.len()];
        let mut next = 0;
        for (id, keep) in reachable.iter().enumerate() {
            if *keep {
                indices[id] = next;
                next += 1;
            }
        }
        fn remap(value: &mut Value, indices: &[usize]) {
            if let Value::Ref(id) = value {
                *id = indices[*id];
            }
        }
        remap(&mut self.root, &indices);
        let mut nodes = Vec::with_capacity(count);
        for (id, mut node) in self.nodes.into_iter().enumerate() {
            if !reachable[id] {
                continue;
            }
            match &mut node {
                Container::Object(entries) => {
                    for (_, value) in entries {
                        remap(value, &indices);
                    }
                }
                Container::Array(items) | Container::Set(items) => {
                    for value in items {
                        remap(value, &indices);
                    }
                }
                Container::Map(entries) => {
                    for (key, value) in entries {
                        remap(key, &indices);
                        remap(value, &indices);
                    }
                }
            }
            nodes.push(node);
        }
        self.nodes = nodes;
        self
    }
    /// Convert a tree-shaped JS-compatible value, retaining UTF-16 and binary64.
    /// Shared acyclic aliases are copied; cycles and Map/Set/Symbol are explicit errors.
    pub fn to_js_value(&self) -> Result<JsValue, ConversionError> {
        crate::stack::with_large_stack(self.nodes.len() > 128, || {
            self.convert(&self.root, &mut Vec::new())
        })
    }
    fn convert(&self, value: &Value, active: &mut Vec<usize>) -> Result<JsValue, ConversionError> {
        Ok(match value {
            Value::Undefined => JsValue::Undefined,
            Value::Null => JsValue::Null,
            Value::Bool(v) => JsValue::Bool(*v),
            Value::Number(v) => JsValue::Number(*v),
            Value::String(v) => JsValue::String(v.as_ref().clone()),
            Value::Symbol { .. } => {
                return Err(ConversionError::new(
                    "TypeError",
                    "Symbol cannot be represented by JsValue",
                ));
            }
            Value::Ref(id) => {
                if active.len() >= crate::stack::CONVERSION_DEPTH {
                    return Err(crate::stack::conversion_exhausted());
                }
                if active.contains(id) {
                    return Err(ConversionError::new(
                        "TypeError",
                        "Cyclic alias graph cannot be represented by JsValue",
                    ));
                }
                active.push(*id);
                let value = match &self.nodes[*id] {
                    Container::Object(entries) => {
                        let mut object = JsObject::new();
                        for (k, v) in entries {
                            object.insert(k.clone(), self.convert(v, active)?);
                        }
                        JsValue::Object(object)
                    }
                    Container::Array(items) => JsValue::Array(
                        items
                            .iter()
                            .map(|v| self.convert(v, active))
                            .collect::<Result<_, _>>()?,
                    ),
                    _ => {
                        return Err(ConversionError::new(
                            "TypeError",
                            "Map or Set cannot be represented by JsValue",
                        ));
                    }
                };
                active.pop();
                value
            }
        })
    }
}
