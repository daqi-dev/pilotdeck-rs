//! Node 22 / V8 12.4 diagnostics for provider Response.json(). Valid values
//! still use the shared codec; failures use the protocol's public V8 diagnostic
//! scanner, which is the same algorithm and the single source of truth.
use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsValue, json_syntax, parse_json};

pub(super) fn parse_response(bytes: &[u8]) -> Result<JsValue, HostError> {
    // Node 22's undici strips a byte BOM, then TextDecoder strips one more.
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    parse_json(text).map_err(|error| {
        let units: Vec<u16> = text.encode_utf16().collect();
        let message =
            json_syntax::parse_error_units(&units).unwrap_or_else(|| error.to_string().into());
        ModuleError::new(message).into()
    })
}
