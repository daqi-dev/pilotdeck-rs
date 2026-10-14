// Port of yaml@2.9.0 dist/nodes/toJS.js (ISC).
use super::{NodeId, NodeKind, ScalarValue};
use crate::{
    doc::Document,
    value::{Container, ConversionError, Value, ValueGraph},
};
use pilotdeck_protocol::JsString;
use std::collections::HashMap;
pub(crate) fn scalar_string(v: &ScalarValue) -> JsString {
    match v {
        ScalarValue::Null => "null".into(),
        ScalarValue::Bool(b) => if *b { "true" } else { "false" }.into(),
        ScalarValue::Number(n) => number_string(*n),
        ScalarValue::String(s) => s.as_ref().clone(),
        ScalarValue::Merge => "Symbol(<<)".into(),
        ScalarValue::Timestamp(ms) => date_string(*ms),
        ScalarValue::Binary(_) => "[object Uint8Array]".into(),
    }
}
pub(crate) fn number_string(n: f64) -> JsString {
    if n.is_nan() {
        "NaN".into()
    } else if n == f64::INFINITY {
        "Infinity".into()
    } else if n == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        ryu_js::Buffer::new().format(n).into()
    }
}
pub(crate) fn date_string(ms: i64) -> JsString {
    use chrono::{Datelike, Timelike};
    let date = chrono::DateTime::from_timestamp_millis(ms).unwrap();
    let year = date.year();
    let year = if year >= 10000 {
        format!("+{year:06}")
    } else if year < 0 {
        format!("-{:06}", -year)
    } else {
        format!("{year:04}")
    };
    format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        date.month(),
        date.day(),
        date.hour(),
        date.minute(),
        date.second(),
        date.timestamp_subsec_millis()
    )
    .into()
}
#[derive(Clone)]
pub(crate) struct Anchor {
    pub(crate) count: usize,
    pub(crate) alias_count: usize,
    pub(crate) res: Value,
}
pub(crate) struct Context<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) nodes: Vec<Container>,
    pub(crate) anchors: HashMap<NodeId, Anchor>,
    pub(crate) alias_sources: HashMap<NodeId, NodeId>,
    pub(crate) anchor_names: std::collections::HashSet<JsString>,
    pub(crate) key_indices: HashMap<usize, HashMap<super::key::Key, usize>>,
    pub(crate) merge_stack: Vec<NodeId>,
    pub(crate) depth: usize,
}
pub(crate) fn to_js(doc: &Document) -> Result<ValueGraph, ConversionError> {
    let mut ctx = Context {
        doc,
        nodes: Vec::new(),
        anchors: HashMap::new(),
        alias_sources: HashMap::new(),
        anchor_names: std::collections::HashSet::new(),
        key_indices: HashMap::new(),
        merge_stack: Vec::new(),
        depth: 0,
    };
    if let Some(root) = doc.contents {
        ctx.visit_order(root)?;
    }
    let root = ctx.convert(doc.contents)?;
    Ok(ValueGraph {
        root,
        nodes: ctx.nodes,
    }
    .retain_reachable())
}
impl Context<'_> {
    pub(crate) fn allocate(&mut self, c: Container) -> Value {
        let id = self.nodes.len();
        self.nodes.push(c);
        Value::Ref(id)
    }

    pub(crate) fn convert(&mut self, id: Option<NodeId>) -> Result<Value, ConversionError> {
        if self.depth + self.merge_stack.len() >= crate::stack::CONVERSION_DEPTH {
            return Err(crate::stack::conversion_exhausted());
        }
        self.depth += 1;
        let result = self.convert_inner(id);
        self.depth -= 1;
        result
    }
    fn convert_inner(&mut self, id: Option<NodeId>) -> Result<Value, ConversionError> {
        let Some(id) = id else { return Ok(Value::Null) };
        let kind = self.doc.nodes[id].kind.clone();
        if let NodeKind::Alias(name) = &kind {
            return self.alias(id, name);
        }
        let value = match kind {
            NodeKind::Scalar(value) => {
                let result = match value {
                    ScalarValue::Null => Value::Null,
                    ScalarValue::Bool(v) => Value::Bool(v),
                    ScalarValue::Number(v) => Value::Number(v),
                    ScalarValue::String(v) => Value::String(v),
                    ScalarValue::Timestamp(ms) => {
                        Value::String(std::sync::Arc::new(date_string(ms)))
                    }
                    ScalarValue::Merge => Value::Symbol {
                        id,
                        description: "<<".into(),
                    },
                    ScalarValue::Binary(data) => {
                        let obj = self.allocate(Container::Object(Vec::new()));
                        self.anchored(id, &obj);
                        let arr = self.allocate(Container::Array(
                            data.into_iter().map(|v| Value::Number(v as f64)).collect(),
                        ));
                        let Value::Ref(obj_id) = obj else {
                            unreachable!()
                        };
                        self.nodes[obj_id] = Container::Object(vec![
                            (
                                "type".into(),
                                Value::String(std::sync::Arc::new("Buffer".into())),
                            ),
                            ("data".into(), arr),
                        ]);
                        return Ok(obj);
                    }
                };
                self.anchored(id, &result);
                result
            }
            NodeKind::Seq(items) => {
                let value = self.allocate(Container::Array(Vec::new()));
                self.anchored(id, &value);
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.convert(Some(item))?);
                }
                let Value::Ref(index) = value else {
                    unreachable!()
                };
                self.nodes[index] = Container::Array(out);
                value
            }
            NodeKind::Map(pairs) | NodeKind::Set(pairs) | NodeKind::OMap(pairs) => {
                let c = match self.doc.nodes[id].kind {
                    NodeKind::Set(_) => Container::Set(Vec::with_capacity(pairs.len())),
                    NodeKind::OMap(_) => Container::Map(Vec::with_capacity(pairs.len())),
                    _ => Container::Object(Vec::with_capacity(pairs.len())),
                };
                let value = self.allocate(c);
                self.anchored(id, &value);
                let Value::Ref(index) = value else {
                    unreachable!()
                };
                for pair in &pairs {
                    if matches!(self.doc.nodes[id].kind, NodeKind::OMap(_)) {
                        let key = self.convert(Some(pair.key))?;
                        let value = self.convert(pair.value)?;
                        if self.has_key(index, &key) {
                            return Err(ConversionError::new(
                                "Error",
                                "Ordered maps must not include duplicate keys",
                            ));
                        }
                        self.insert(index, key, value, false);
                    } else {
                        self.add_pair(index, pair)?;
                    }
                }
                value
            }
            NodeKind::Pairs(pairs) => {
                let value = self.allocate(Container::Array(Vec::new()));
                self.anchored(id, &value);
                let Value::Ref(index) = value else {
                    unreachable!()
                };
                let mut out = Vec::new();
                for pair in &pairs {
                    let obj = self.allocate(Container::Object(Vec::new()));
                    let Value::Ref(i) = obj else { unreachable!() };
                    self.add_pair(i, pair)?;
                    out.push(obj);
                }
                self.nodes[index] = Container::Array(out);
                value
            }
            NodeKind::Alias(_) => unreachable!(),
        };
        Ok(value)
    }
}
pub(crate) fn same_value_zero(a: &Value, b: &Value) -> bool {
    a == b || matches!((a,b),(Value::Number(a),Value::Number(b))if a.is_nan()&&b.is_nan())
}
