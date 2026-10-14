//! JS value graph. References preserve alias identity and cycles without Rc cycles.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
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
    /// Convert a tree-shaped JS-compatible value, retaining UTF-16 and binary64.
    /// Shared acyclic aliases are copied; cycles and Map/Set/Symbol are explicit errors.
    pub fn to_js_value(&self) -> Result<JsValue, ConversionError> {
        crate::stack::with_large_stack(self.nodes.len() > 128, || {
            self.convert(&self.root, &mut Vec::new())
        })
    }
    fn convert(&self, value: &Value, active: &mut Vec<usize>) -> Result<JsValue, ConversionError> {
        Ok(match value {
            Value::Null => JsValue::Null,
            Value::Bool(v) => JsValue::Bool(*v),
            Value::Number(v) => JsValue::Number(*v),
            Value::String(v) => JsValue::String(v.clone()),
            Value::Symbol { .. } => {
                return Err(ConversionError::new(
                    "TypeError",
                    "Symbol cannot be represented by JsValue",
                ));
            }
            Value::Ref(id) => {
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
