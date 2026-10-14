// Port of yaml@2.9.0 dist/nodes/Scalar.js (ISC), stored value types.
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug, PartialEq)]
pub enum ScalarValue {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Binary(Vec<u8>),
    Timestamp(i64),
    Merge,
}
