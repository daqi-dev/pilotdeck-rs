//! Settings are resolved and updated on the Gateway host, never on the CLI host.
use super::{Connection, transport};
use crate::{Failure, json_pretty, stdout_line};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use serde_json::value::RawValue;

pub(super) fn parse(raw: &str) -> Result<Box<RawValue>, Failure> {
    let settings = parse_json(raw).map_err(|e| Failure::Error(e.to_string()))?;
    if !matches!(settings, JsValue::Object(_)) {
        return Err(Failure::Error(
            "updateSettings settings must be an object.".into(),
        ));
    }
    // JSON.parse rounds numbers as JS doubles; JSON.stringify maps
    // non-finite values to null and preserves duplicate-key semantics.
    let encoded = stringify_json(&settings)
        .map_err(|e| Failure::Error(e.to_string()))?
        .unwrap_or_else(|| "null".into());
    RawValue::from_string(encoded).map_err(|e| Failure::Error(e.to_string()))
}

pub(super) fn get(connection: &Connection) -> Result<u8, Failure> {
    let result = request(
        connection,
        "resolve_settings",
        RawValue::from_string("{}".into()).map_err(|e| Failure::Error(e.to_string()))?,
    )?;
    let raw = stringify_json(&result)
        .map_err(|e| Failure::Error(e.to_string()))?
        .unwrap_or_else(|| "undefined".into());
    stdout_line(&json_pretty(&raw));
    Ok(0)
}

pub(super) fn set(connection: &Connection, settings: Box<RawValue>) -> Result<u8, Failure> {
    request(
        connection,
        "update_settings",
        RawValue::from_string(format!(
            "{{\"source\":\"localSettings\",\"settings\":{settings}}}"
        ))
        .map_err(|e| Failure::Error(e.to_string()))?,
    )?;
    stdout_line("{\"updated\":true}");
    Ok(0)
}

fn request(
    connection: &Connection,
    method: &str,
    params: Box<RawValue>,
) -> Result<JsValue, Failure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::Error(e.to_string()))?;
    runtime.block_on(transport::request_with_version(
        connection, method, params, "0.1.0",
    ))
}
