//! MCP tool naming and text hygiene, ported from the fixed TypeScript
//! baseline:
//!
//! - `src/mcp/runtime/sanitize.ts` (M9): strip ambient/bidi code points from
//!   every string (and object key) that came from the network;
//! - `src/mcp/runtime/wireName.ts` (M10): `mcp__<server>__<tool>` with both
//!   segments mapped to `[A-Za-z0-9_-]`;
//! - `src/mcp/runtime/truncate.ts` (M11): clamp descriptions to 2048 UTF-16
//!   code units.
//!
//! All text is handled as UTF-16 code units ([`JsString`]) so lone surrogates
//! and astral characters behave exactly as in JS (`value.length`, `slice`,
//! `/[^A-Za-z0-9_-]/g`).
//!
//! The baseline has no collision suffix: two tool names that normalise to the
//! same segment (`a.b`, `a b`) produce the same wire name. That is preserved
//! here; resolving collisions belongs to the tool bridge (A6.5-pr10b).

use crate::js_value::{MAX_ARRAY_DEPTH, StackOverflow};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// `MAX_MCP_TOOL_DESCRIPTION_LENGTH`.
pub const MAX_MCP_TOOL_DESCRIPTION_LENGTH: usize = 2048;
const TRUNCATED_SUFFIX: &str = "\u{2026} [truncated]";

/// V8's `RangeError: Maximum call stack size exceeded` from walking a tree
/// nested deeper than [`MAX_ARRAY_DEPTH`] (the TS helpers recurse).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallStackExceeded;
impl CallStackExceeded {
    pub const MESSAGE: &'static str = StackOverflow::MESSAGE;
}
impl From<StackOverflow> for CallStackExceeded {
    fn from(_: StackOverflow) -> Self {
        Self
    }
}
impl std::fmt::Display for CallStackExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(Self::MESSAGE)
    }
}
impl std::error::Error for CallStackExceeded {}

/// `STRIPPED_CODE_POINTS` (all BMP, none a surrogate, so a code-unit filter
/// equals the TS code-point loop).
const STRIPPED: [u16; 17] = [
    0x200B, 0x200C, 0x200D, 0x200E, 0x200F, 0x202A, 0x202B, 0x202C, 0x202D, 0x202E, 0x2066, 0x2067,
    0x2068, 0x2069, 0xFEFF, 0xFFFC, 0xFFFD,
];

/// `sanitizeUnicodeString`.
pub fn sanitize_unicode_string(value: &JsString) -> JsString {
    JsString::from_utf16(
        value
            .units()
            .iter()
            .copied()
            .filter(|unit| !STRIPPED.contains(unit))
            .collect(),
    )
}

/// `recursivelySanitizeUnicode`.
///
/// Objects are rebuilt in `Object.entries` order with `out[key] = value`, so a
/// key that sanitises onto an earlier one overwrites its value in place, and a
/// key that sanitises to `__proto__` sets the prototype instead of an own
/// property (it disappears from the result). The walk is iterative; past
/// [`MAX_ARRAY_DEPTH`] nesting it reports V8's `RangeError` instead of
/// recursing (the TS recursion overflows the JS stack at a similar depth).
pub fn recursively_sanitize_unicode(value: &JsValue) -> Result<JsValue, CallStackExceeded> {
    map_tree(
        value,
        |key| {
            let key = sanitize_unicode_string(key);
            (!key.equals_str("__proto__")).then_some(key)
        },
        |text| JsValue::String(sanitize_unicode_string(text)),
    )
}

/// Rebuilds a JSON-shaped tree: strings through `string`, object keys through
/// `key` (`None` drops the property, after its value is still evaluated as in
/// the TS recursion), in `Object.entries` order. Iterative, depth-bounded.
pub(crate) fn map_tree(
    value: &JsValue,
    key: impl Fn(&JsString) -> Option<JsString>,
    string: impl Fn(&JsString) -> JsValue,
) -> Result<JsValue, CallStackExceeded> {
    type Entries<'a> = std::vec::IntoIter<(&'a JsString, &'a JsValue)>;
    enum Frame<'a> {
        Array(std::slice::Iter<'a, JsValue>, Vec<JsValue>),
        Object(Entries<'a>, JsObject, Option<JsString>),
    }
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut current = value;
    'descend: loop {
        let mut done = match current {
            JsValue::Array(items) => {
                if stack.len() >= MAX_ARRAY_DEPTH {
                    return Err(CallStackExceeded);
                }
                stack.push(Frame::Array(items.iter(), Vec::with_capacity(items.len())));
                None
            }
            JsValue::Object(object) => {
                if stack.len() >= MAX_ARRAY_DEPTH {
                    return Err(CallStackExceeded);
                }
                let entries = object.own_entries().into_iter();
                stack.push(Frame::Object(entries, JsObject::new(), None));
                None
            }
            JsValue::String(text) => Some(string(text)),
            other => Some(other.clone()),
        };
        loop {
            if let Some(finished) = done.take() {
                match stack.last_mut() {
                    None => return Ok(finished),
                    Some(Frame::Array(_, out)) => out.push(finished),
                    Some(Frame::Object(_, out, pending)) => {
                        if let Some(name) = pending.take() {
                            out.insert(name, finished);
                        }
                    }
                }
            }
            match stack.last_mut() {
                None => unreachable!("a finished root returns above"),
                Some(Frame::Array(items, _)) => {
                    if let Some(item) = items.next() {
                        current = item;
                        continue 'descend;
                    }
                }
                Some(Frame::Object(entries, _, pending)) => {
                    if let Some((name, item)) = entries.next() {
                        *pending = key(name);
                        current = item;
                        continue 'descend;
                    }
                }
            }
            done = match stack.pop() {
                Some(Frame::Array(_, out)) => Some(JsValue::Array(out)),
                Some(Frame::Object(_, out, _)) => Some(JsValue::Object(out)),
                None => unreachable!("checked above"),
            };
        }
    }
}

fn normalize_segment(units: &[u16], out: &mut Vec<u16>) {
    out.extend(units.iter().map(|unit| match *unit {
        0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A | 0x5F | 0x2D => *unit,
        _ => u16::from(b'_'),
    }));
}

/// `buildMcpToolWireName`.
pub fn build_mcp_tool_wire_name(server_id: &JsString, tool_name: &JsString) -> JsString {
    let mut out: Vec<u16> = "mcp__".encode_utf16().collect();
    normalize_segment(server_id.units(), &mut out);
    out.extend("__".encode_utf16());
    normalize_segment(tool_name.units(), &mut out);
    JsString::from_utf16(out)
}

/// `parseMcpToolWireName`: `(serverId, toolName)` or `None`.
pub fn parse_mcp_tool_wire_name(wire_name: &JsString) -> Option<(JsString, JsString)> {
    let prefix: Vec<u16> = "mcp__".encode_utf16().collect();
    let rest = wire_name.units().strip_prefix(prefix.as_slice())?;
    let underscore = u16::from(b'_');
    let sep = rest
        .windows(2)
        .position(|pair| pair == [underscore, underscore])?;
    if sep == 0 {
        return None;
    }
    let server = &rest[..sep];
    let tool = &rest[sep + 2..];
    if tool.is_empty() {
        return None;
    }
    Some((
        JsString::from_utf16(server.to_vec()),
        JsString::from_utf16(tool.to_vec()),
    ))
}

/// `truncateMcpToolDescription` (UTF-16 `slice`, may split a surrogate pair).
pub fn truncate_mcp_tool_description(value: &JsString) -> JsString {
    let units = value.units();
    if units.len() <= MAX_MCP_TOOL_DESCRIPTION_LENGTH {
        return value.clone();
    }
    let mut out = units[..MAX_MCP_TOOL_DESCRIPTION_LENGTH].to_vec();
    out.extend(TRUNCATED_SUFFIX.encode_utf16());
    JsString::from_utf16(out)
}
