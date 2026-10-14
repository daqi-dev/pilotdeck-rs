//! Boundary compatibility for the fixed PilotDeck product. No host authority or IO.
//! Module 2.0 and Gateway 1.2 are deliberately separate protocols.

use std::{collections::HashMap, fmt};

pub mod admission;
mod codec;
mod consumer;
pub mod model;
pub mod module;
mod snapshots;
pub mod wire;

pub const MODULE_PROTOCOL_VERSION: &str = "2.0";
pub const GATEWAY_PROTOCOL_VERSION: &str = "1.2";

/// JavaScript strings contain UTF-16 code units, including unpaired surrogates.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JsString(Vec<u16>);

impl JsString {
    pub fn from_utf16(units: Vec<u16>) -> Self {
        Self(units)
    }
    pub fn units(&self) -> &[u16] {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len_utf16(&self) -> usize {
        self.0.len()
    }
    /// Explicitly fallible: identifiers are never silently replacement-decoded.
    pub fn to_utf8(&self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.0)
    }
    pub fn equals_str(&self, value: &str) -> bool {
        self.0.iter().copied().eq(value.encode_utf16())
    }
}
impl From<&str> for JsString {
    fn from(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }
}
impl From<String> for JsString {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}
impl fmt::Debug for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// First insertion owns property order; later writes replace its value.
#[derive(Clone, Default, Debug)]
pub struct JsObject {
    entries: Vec<(JsString, JsValue)>,
    indices: HashMap<JsString, usize>,
    prototype: Option<Box<JsValue>>,
    native_tag: Option<&'static str>,
}
impl JsObject {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, name: &str) -> Option<&JsValue> {
        self.get_key(&JsString::from(name))
    }
    pub fn get_key(&self, name: &JsString) -> Option<&JsValue> {
        self.indices.get(name).map(|index| &self.entries[*index].1)
    }
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    pub fn insert(&mut self, name: JsString, value: JsValue) -> Option<JsValue> {
        if let Some(index) = self.indices.get(&name) {
            return Some(std::mem::replace(&mut self.entries[*index].1, value));
        }
        self.indices.insert(name.clone(), self.entries.len());
        self.entries.push((name, value));
        None
    }
    /// Configuration-only prototype assignment. Own-property inspection and
    /// JSON encoding remain unchanged; property lookup is explicitly opt-in.
    pub fn set_prototype(&mut self, value: JsValue) {
        self.prototype = Some(Box::new(value));
    }
    /// Native Map/Set toString tag for configuration inspection; not an own key.
    pub fn set_native_tag(&mut self, tag: &'static str) {
        self.native_tag = Some(tag);
    }
    pub fn native_tag(&self) -> Option<&'static str> {
        self.native_tag
    }
    pub fn prototype(&self) -> Option<&JsValue> {
        self.prototype.as_deref()
    }
    pub fn get_property(&self, name: &str) -> Option<&JsValue> {
        let mut object = self;
        loop {
            if let Some(value) = object.get(name) {
                return Some(value);
            }
            object = object.prototype.as_deref()?.as_object()?;
        }
    }
    /// Insertion order, for lossless boundary inspection. Stringify uses JS key order.
    pub fn entries(&self) -> &[(JsString, JsValue)] {
        &self.entries
    }
}
impl PartialEq for JsObject {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

/// A wire boundary value, not the application state model.
/// Missing is represented by object lookup returning None; Undefined is present.
pub enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Array(Vec<JsValue>),
    Object(JsObject),
}
impl JsValue {
    pub fn as_object(&self) -> Option<&JsObject> {
        if let Self::Object(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_string(&self) -> Option<&JsString> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_number(&self) -> Option<f64> {
        if let Self::Number(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn get(&self, name: &str) -> Option<&JsValue> {
        self.as_object()?.get(name)
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self, Self::Undefined)
    }
}
impl From<&str> for JsValue {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}
impl Clone for JsValue {
    fn clone(&self) -> Self {
        codec::clone_value(self)
    }
}
/// Structural insertion-entry comparison with f64 numeric equality.
/// This is NOT the JSON.stringify identity used by ordered-wire replay.
impl PartialEq for JsValue {
    fn eq(&self, other: &Self) -> bool {
        let mut pending = vec![(self, other)];
        while let Some((left, right)) = pending.pop() {
            match (left, right) {
                (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => (),
                (Self::Bool(a), Self::Bool(b)) if a == b => (),
                (Self::Number(a), Self::Number(b)) if a == b => (),
                (Self::String(a), Self::String(b)) if a == b => (),
                (Self::Array(a), Self::Array(b)) if a.len() == b.len() => {
                    pending.extend(a.iter().zip(b));
                }
                (Self::Object(a), Self::Object(b)) if a.len() == b.len() => {
                    for ((ak, av), (bk, bv)) in a.entries.iter().zip(&b.entries) {
                        if ak != bk {
                            return false;
                        }
                        pending.push((av, bv));
                    }
                }
                _ => return false,
            }
        }
        true
    }
}
impl fmt::Debug for JsValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match stringify_json(self) {
            Ok(Some(text)) => f.write_str(&text),
            Ok(None) => f.write_str("undefined"),
            Err(error) => write!(f, "<JSON error: {error}>"),
        }
    }
}
impl Drop for JsValue {
    fn drop(&mut self) {
        // Empty containers before each ordinary Drop. Children are destroyed iteratively.
        fn detach(value: &mut JsValue, pending: &mut Vec<JsValue>) {
            match value {
                JsValue::Array(children) => pending.append(children),
                JsValue::Object(object) => {
                    pending.extend(object.entries.drain(..).map(|(_, value)| value));
                    object.indices.clear();
                    if let Some(prototype) = object.prototype.take() {
                        pending.push(*prototype);
                    }
                }
                _ => (),
            }
        }
        let mut pending = Vec::new();
        detach(self, &mut pending);
        while let Some(mut child) = pending.pop() {
            detach(&mut child, &mut pending);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecError {
    pub offset: usize,
    pub message: String,
}
impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}
impl std::error::Error for CodecError {}

/// Parse legal JSON using original JS numeric/string/property semantics.
pub fn parse_json(raw: &str) -> Result<JsValue, CodecError> {
    codec::parse(raw)
}
/// Top-level Undefined has no JSON text; array Undefined becomes null, object omits it.
pub fn stringify_json(value: &JsValue) -> Result<Option<String>, CodecError> {
    codec::stringify(value)
}

#[cfg(test)]
mod prototype_tests {
    use super::*;
    #[test]
    fn prototypes_are_explicit_and_do_not_change_wire_properties() {
        let mut parent = JsObject::new();
        parent.insert("inherited".into(), "value".into());
        let mut child = JsObject::new();
        child.insert("own".into(), "value".into());
        child.set_prototype(JsValue::Object(parent));
        assert!(child.get("inherited").is_none());
        assert_eq!(
            child.get_property("inherited"),
            Some(&JsValue::from("value"))
        );
        let cloned = child.clone();
        assert_eq!(
            cloned.get_property("inherited"),
            child.get_property("inherited")
        );
        child.set_native_tag("Map");
        let value = JsValue::Object(child);
        let cloned = value.clone();
        assert_eq!(cloned.as_object().unwrap().native_tag(), Some("Map"));
        assert_eq!(
            cloned.as_object().unwrap().get_property("inherited"),
            Some(&JsValue::from("value"))
        );
        assert_eq!(
            stringify_json(&value).unwrap().unwrap(),
            "{\"own\":\"value\"}"
        );
    }
}
