use pilotdeck_protocol::JsValue;
pub const MAX_ARRAY_DEPTH: usize = 2000;
#[derive(Debug)]
pub struct StackOverflow;
impl StackOverflow {
    pub const MESSAGE: &'static str = "Maximum call stack size exceeded";
}
fn number_string(n: f64) -> String {
    pilotdeck_protocol::number::number_to_string(n)
}
pub fn to_string_units(value: &JsValue) -> Result<Vec<u16>, StackOverflow> {
    let mut out = Vec::new();
    // One frame per array being joined: its remaining items and whether a
    // separator is due before the next one.
    let mut frames: Vec<(std::slice::Iter<'_, JsValue>, bool)> = Vec::new();
    let mut next = Some(value);
    loop {
        match next.take() {
            Some(JsValue::Array(items)) => {
                if frames.len() >= MAX_ARRAY_DEPTH {
                    return Err(StackOverflow);
                }
                frames.push((items.iter(), false));
            }
            Some(scalar) => scalar_units(scalar, &mut out),
            None => {}
        }
        let Some((items, separate)) = frames.last_mut() else {
            return Ok(out);
        };
        match items.next() {
            Some(item) => {
                if *separate {
                    out.push(u16::from(b','));
                }
                *separate = true;
                if !matches!(item, JsValue::Undefined | JsValue::Null) {
                    next = Some(item);
                }
            }
            None => {
                frames.pop();
            }
        }
    }
}
fn scalar_units(value: &JsValue, out: &mut Vec<u16>) {
    match value {
        JsValue::String(s) => out.extend_from_slice(s.units()),
        JsValue::Undefined => out.extend("undefined".encode_utf16()),
        JsValue::Null => out.extend("null".encode_utf16()),
        JsValue::Bool(b) => out.extend(b.to_string().encode_utf16()),
        JsValue::Number(n) => out.extend(number_string(*n).encode_utf16()),
        // `String(new Set())` is `[object Set]`: the tag of a collapsed YAML
        // collection survives into `JsValue`, and dropping it here made an
        // array join print `[object Object]` where TS keeps the class
        // (#1550 修 r6).
        JsValue::Object(o) => {
            out.extend("[object ".encode_utf16());
            out.extend(o.native_tag().unwrap_or("Object").encode_utf16());
            out.push(u16::from(b']'));
        }
        JsValue::Array(_) => unreachable!("arrays are joined by to_string_units"),
    }
}
