//! Hash keys for ordered JS tables; numeric keys use SameValueZero.
use super::ScalarValue;
use crate::value::Value;
use pilotdeck_protocol::JsString;
use std::sync::Arc;

#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) enum Key {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(Arc<JsString>),
    Symbol(usize),
    Ref(usize),
}
impl Key {
    fn number(n: f64) -> Self {
        Self::Number(if n == 0.0 {
            0
        } else if n.is_nan() {
            f64::NAN.to_bits()
        } else {
            n.to_bits()
        })
    }
    pub(crate) fn scalar(value: &ScalarValue) -> Option<Self> {
        Some(match value {
            ScalarValue::Null => Self::Null,
            ScalarValue::Bool(b) => Self::Bool(*b),
            ScalarValue::Number(n) => Self::number(*n),
            ScalarValue::String(s) => Self::String(s.clone()),
            _ => return None,
        })
    }
}
impl From<&Value> for Key {
    fn from(value: &Value) -> Self {
        match value {
            Value::Undefined => Self::Undefined,
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Number(n) => Self::number(*n),
            Value::String(s) => Self::String(s.clone()),
            Value::Symbol { id, .. } => Self::Symbol(*id),
            Value::Ref(id) => Self::Ref(*id),
        }
    }
}
