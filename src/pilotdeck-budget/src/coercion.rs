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

use pilotdeck_protocol::stringify_json;
fn display(value: &JsValue) -> Result<String, StackOverflow> {
    Ok(String::from_utf16_lossy(&to_string_units(value)?))
}
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}
pub fn json_stringify(value: &JsValue) -> Result<Option<String>, StackOverflow> {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_ARRAY_DEPTH {
            return Err(StackOverflow);
        }
        match value {
            JsValue::Array(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            JsValue::Object(object) => {
                pending.extend(object.entries().iter().map(|(_, item)| (item, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(stringify_json(value).ok().flatten())
}

/// `Number(value)`.
pub fn to_number(value: &JsValue) -> Result<f64, StackOverflow> {
    Ok(match value {
        JsValue::Undefined | JsValue::Object(_) => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(b) => f64::from(u8::from(*b)),
        JsValue::Number(n) => *n,
        JsValue::String(s) => string_to_number(&String::from_utf16_lossy(s.units())),
        JsValue::Array(_) => string_to_number(&display(value)?),
    })
}

/// ECMAScript StringToNumber.
pub fn string_to_number(text: &str) -> f64 {
    let t = js_trim(text);
    if t.is_empty() {
        return 0.0;
    }
    let radix = match t.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 0,
    };
    if radix != 0 {
        let digits = &t[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        // Exact up to 128 bits, then rounded once, like V8.
        return match u128::from_str_radix(digits, radix) {
            Ok(v) => v as f64,
            Err(_) => digits.chars().fold(0.0, |acc, c| {
                acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap())
            }),
        };
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = digits(int) && digits(frac) && !(int.is_empty() && frac.is_empty());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    if mantissa_ok && exponent_ok {
        t.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}
