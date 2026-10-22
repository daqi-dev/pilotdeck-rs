//! Iterative JSON transforms: wire values may be deeper than the native stack.
use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// Conservative lower bound of the depth where V8's recursive `sortValue`
/// (the baseline cache key) always succeeds. Measured on Node v22.22.0 with
/// the same projected cache-key object: transform depth 1892 (schema depth
/// 1890) succeeded and transform depth 1897 (schema depth 1895) failed 5/5
/// times. 1800 keeps the guaranteed-success side exact, fails identically
/// above the measured failure point. The early-rejection band is unapproved:
/// specs/gateway-parity/accepted-diffs.md A6.4-COUNT-CACHE-DEPTH-001.
pub(super) const CACHE_KEY_MAX_DEPTH: usize = 1800;

pub(super) fn transform(
    value: &JsValue,
    sort_keys: bool,
    finish_object: fn(JsObject) -> JsObject,
) -> Result<JsValue, HostError> {
    transform_with_limit(
        value,
        sort_keys,
        crate::js_value::MAX_ARRAY_DEPTH,
        finish_object,
    )
}

pub(super) fn transform_with_limit(
    value: &JsValue,
    sort_keys: bool,
    max_depth: usize,
    finish_object: fn(JsObject) -> JsObject,
) -> Result<JsValue, HostError> {
    enum Frame<'a> {
        Visit(&'a JsValue, usize),
        Array(usize),
        Object(Vec<JsString>),
    }
    let mut frames = vec![Frame::Visit(value, 0)];
    let mut values = Vec::new();
    while let Some(frame) = frames.pop() {
        match frame {
            Frame::Visit(value, depth) => {
                if depth > max_depth {
                    return Err(ModuleError::new(crate::js_value::StackOverflow::MESSAGE).into());
                }
                match value {
                    JsValue::Array(items) => {
                        frames.push(Frame::Array(items.len()));
                        frames.extend(items.iter().rev().map(|v| Frame::Visit(v, depth + 1)));
                    }
                    JsValue::Object(object) => {
                        let mut entries: Vec<_> = object.entries().iter().collect();
                        if sort_keys {
                            entries.sort_by(|(a, _), (b, _)| a.units().cmp(b.units()));
                        }
                        frames.push(Frame::Object(
                            entries.iter().map(|(k, _)| k.clone()).collect(),
                        ));
                        frames.extend(
                            entries
                                .into_iter()
                                .rev()
                                .map(|(_, v)| Frame::Visit(v, depth + 1)),
                        );
                    }
                    _ => values.push(value.clone()),
                }
            }
            Frame::Array(count) => {
                let items = values.split_off(values.len() - count);
                values.push(JsValue::Array(items));
            }
            Frame::Object(keys) => {
                let items = values.split_off(values.len() - keys.len());
                let mut object = JsObject::new();
                for (key, value) in keys.into_iter().zip(items) {
                    object.insert(key, value);
                }
                values.push(JsValue::Object(finish_object(object)));
            }
        }
    }
    Ok(values.pop().expect("one transform result"))
}
