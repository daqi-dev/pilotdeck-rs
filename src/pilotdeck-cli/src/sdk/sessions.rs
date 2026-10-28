//! sessions default/list; messages retains its preflight until D1-P03.
use super::{create_client, positional, read_connection, read_value, transport};
use crate::{Failure, paths::Env, stdout_line};
use pilotdeck_protocol::{JsValue, stringify_json};
use serde_json::json;
use std::path::Path;

pub(super) fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let connection = read_connection(argv, env, cwd)?;
    create_client(&connection)?;
    match argv.first().map_or("list", String::as_str) {
        "list" => {
            let mut params = json!({});
            if let Some(project) = read_value(argv, "--project") {
                params["projectKey"] = project.into();
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Failure::Error(e.to_string()))?;
            let result =
                runtime.block_on(transport::request(&connection, "list_sessions", params))?;
            if matches!(result, JsValue::Null | JsValue::Undefined) {
                let kind = if matches!(result, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                };
                return Err(Failure::Error(format!(
                    "Cannot read properties of {kind} (reading 'sessions')"
                )));
            }
            let empty = JsValue::Array(Vec::new());
            let sessions = result
                .get("sessions")
                .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                .unwrap_or(&empty);
            stdout_line(&pretty_json(sessions)?);
            Ok(0)
        }
        "messages" => {
            if positional(&argv[1..])
                .first()
                .is_none_or(|id| id.is_empty())
            {
                return Err(Failure::Error(
                    "Usage: pilotdeck sessions messages <session-id>".into(),
                ));
            }
            Err(Failure::Todo(
                "pilotdeck sessions messages (SDK sessions.messages)",
            ))
        }
        _ => Err(Failure::Error(
            "Usage: pilotdeck sessions <list|messages> ...".into(),
        )),
    }
}

// First use the existing JS codec (key order, f64 spelling, lone surrogates).
// Add only whitespace outside strings, preserving every encoded scalar byte.
fn pretty_json(value: &JsValue) -> Result<String, Failure> {
    let raw = stringify_json(value)
        .map_err(|e| Failure::Error(e.to_string()))?
        .unwrap_or_else(|| "null".into());
    Ok(crate::json_pretty(&raw))
}
