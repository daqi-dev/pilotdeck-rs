//! Extracted original PilotDeck Anthropic request/stream protocol. No gateway, Kernel, credentials or network authority.
pub mod anthropic;
pub mod blocks;
pub mod errors;
pub mod headers;
pub mod jsonrepair;
pub mod thinking;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
#[derive(Clone, Debug)]
pub enum AdapterError {
    Request {
        code: JsString,
        message: JsString,
        details: Option<Box<JsValue>>,
    },
    Provider(JsObject),
    Thrown {
        name: JsString,
        message: JsString,
    },
}
impl AdapterError {
    pub fn message(&self) -> JsString {
        match self {
            Self::Request { message, .. } | Self::Thrown { message, .. } => message.clone(),
            Self::Provider(value) => value
                .get("message")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default(),
        }
    }
}
pub fn append_js_string(
    buffer: &mut Vec<u16>,
    more: &[u16],
    limit: usize,
) -> Result<(), AdapterError> {
    if buffer.len().saturating_add(more.len()) > limit {
        return Err(AdapterError::Thrown {
            name: "RangeError".into(),
            message: "Invalid string length".into(),
        });
    }
    buffer.extend_from_slice(more);
    Ok(())
}
/// UTF-16 JSON text as a Rust string for `parse_json`. A lone surrogate is
/// only legal inside a JSON string, where `\uXXXX` denotes the same unit; one
/// that would follow an escaping backslash stays invalid (U+FFFD), and
/// anywhere else both forms are a syntax error, as in `JSON.parse`. Shared by
/// the Anthropic and OpenAI tool-argument parsers (pr5).
pub fn json_text(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    let mut backslashes = 0usize;
    for item in char::decode_utf16(units.iter().copied()) {
        match item {
            Ok(c) => {
                out.push(c);
                backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            }
            Err(error) => {
                if backslashes % 2 == 1 {
                    out.push('\u{FFFD}');
                } else {
                    out.push_str(&format!("\\u{:04x}", error.unpaired_surrogate()));
                }
                backslashes = 0;
            }
        }
    }
    out
}

/// `JSON.parse(text)`, else `JSON.parse(jsonrepair(text))`, as every stream
/// normalizer parses tool-call arguments (anthropic, openai, openai-responses).
/// The bool is `wasRepaired`; `None` when both parses fail. Callers handle
/// their own empty-buffer default first.
pub fn parse_tool_arguments(units: &[u16]) -> Option<(JsValue, bool)> {
    let parse = |units: &[u16]| pilotdeck_protocol::parse_json(&json_text(units)).ok();
    if let Some(input) = parse(units) {
        return Some((input, false));
    }
    let repaired = crate::jsonrepair::jsonrepair(units).ok()?;
    parse(&repaired).map(|input| (input, true))
}

pub const SPACE: &str =
    r"\t-\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}";
pub fn retry_after_from_message(message: &str) -> Option<f64> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let lowered = message.to_ascii_lowercase();
    let captures=RE.get_or_init(||regex::Regex::new(&r"(?:try again|retry)\s+in\s+([0-9]+(?:\.[0-9]+)?)\s*(ms|milliseconds?|s|seconds?|minutes?|m)(?-u:\b)".replace("\\s",&format!("[{SPACE}]"))).expect("static regex")).captures(&lowered)?;
    let value: f64 = captures[1].parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let unit = &captures[2];
    let n = if unit == "ms" || unit.starts_with("millisecond") {
        value
    } else if unit == "s" || unit.starts_with("second") {
        value * 1000.0
    } else if unit == "m" || unit.starts_with("minute") {
        value * 60000.0
    } else {
        return None;
    };
    let floor = n.floor();
    Some(if n - floor >= 0.5 { floor + 1.0 } else { floor })
}
