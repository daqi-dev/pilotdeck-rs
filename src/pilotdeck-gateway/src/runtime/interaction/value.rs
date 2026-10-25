//! Producer-owned values before the JSON-shaped pending/replay boundary.
use super::{reconnect::replay_payload, *};
use std::collections::HashSet;

/// An arena node; array elements / object fields refer to indices in the same
/// slice. Repeated indices preserve host identity without owning cyclic Rust
/// references. Map, Set and class instances supply only their own enumerable
/// string fields as Object, never their internal entries or prototype methods.
/// Producers enumerate properties before calling; this API runs no host code.
#[derive(Debug)]
pub enum ProducerValue {
    Value(JsValue),
    /// Epoch milliseconds (TimeClip), or NaN for an invalid Date.
    Date(f64),
    /// Canonical decimal digits, optional minus; no leading zero or negative zero.
    BigInt(String),
    Function,
    Symbol,
    Array(Vec<usize>),
    /// Unique own keys, in insertion order; integer keys are sorted on traversal.
    Object(Vec<(JsString, usize)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayValueError {
    pub name: &'static str,
    pub message: JsString,
}
impl ReplayValueError {
    fn new(name: &'static str, message: &str) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }
}
impl From<JsString> for ReplayValueError {
    fn from(message: JsString) -> Self {
        Self {
            name: "Error",
            message,
        }
    }
}

/// TS toInteractionReplayValue: None means omitted payload, not conversion
/// failure. Invalid Dates and malformed producer graphs return explicit errors.
/// Each call has a fresh seen set; repeats (not just back edges) are circular.
pub fn to_interaction_replay_value(
    nodes: &[ProducerValue],
    root: usize,
) -> Result<Option<JsValue>, ReplayValueError> {
    enum Work<'a> {
        Visit(usize),
        Array(usize),
        Object(Vec<&'a JsString>),
    }
    let mut work = vec![Work::Visit(root)];
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    while let Some(task) = work.pop() {
        match task {
            Work::Visit(index) => {
                let node = nodes.get(index).ok_or_else(|| {
                    ReplayValueError::new("TypeError", "Invalid interaction producer reference")
                })?;
                match node {
                    ProducerValue::Array(_)
                    | ProducerValue::Object(_)
                    | ProducerValue::Value(JsValue::Array(_) | JsValue::Object(_))
                        if !seen.insert(index) =>
                    {
                        values.push("[Circular]".into());
                    }
                    ProducerValue::Value(value) => values
                        .push(replay_payload(Some(value.clone())).unwrap_or(JsValue::Undefined)),
                    ProducerValue::Function | ProducerValue::Symbol => {
                        values.push(JsValue::Undefined)
                    }
                    ProducerValue::Date(ms) => {
                        if !ms.is_finite() || ms.abs() > 8.64e15 {
                            return Err(ReplayValueError::new("RangeError", "Invalid time value"));
                        }
                        values.push(JsValue::String(
                            pilotdeck_transport::system_clock::format_epoch_iso(*ms as i64).into(),
                        ));
                    }
                    ProducerValue::BigInt(decimal) => {
                        let digits = decimal.strip_prefix('-').unwrap_or(decimal);
                        if digits.is_empty()
                            || !digits.bytes().all(|b| b.is_ascii_digit())
                            || (digits.starts_with('0') && decimal != "0")
                        {
                            return Err(ReplayValueError::new(
                                "TypeError",
                                "Invalid interaction producer BigInt",
                            ));
                        }
                        values.push(JsValue::String(decimal.clone().into()));
                    }
                    ProducerValue::Array(items) => {
                        work.push(Work::Array(items.len()));
                        work.extend(items.iter().rev().map(|&i| Work::Visit(i)));
                    }
                    ProducerValue::Object(fields) => {
                        let mut fields: Vec<_> = fields.iter().collect();
                        fields.sort_by_key(|(key, _)| {
                            (key.array_index().is_none(), key.array_index())
                        });
                        work.push(Work::Object(fields.iter().map(|(key, _)| key).collect()));
                        work.extend(fields.into_iter().rev().map(|(_, i)| Work::Visit(*i)));
                    }
                }
            }
            Work::Array(count) => {
                let items = values
                    .split_off(values.len() - count)
                    .into_iter()
                    .map(|v| if v.is_undefined() { JsValue::Null } else { v })
                    .collect();
                values.push(JsValue::Array(items));
            }
            Work::Object(keys) => {
                let children = values.split_off(values.len() - keys.len());
                let mut object = JsObject::new();
                for (key, child) in keys.into_iter().zip(children) {
                    // TS visits __proto__ before assignment, including errors/seen.
                    if !key.equals_str("__proto__") && !child.is_undefined() {
                        object.insert(key.clone(), child);
                    }
                }
                values.push(JsValue::Object(object));
            }
        }
    }
    Ok(values.pop().filter(|value| !value.is_undefined()))
}

#[cfg(test)]
mod tests;
