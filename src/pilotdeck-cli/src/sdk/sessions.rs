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
    let mut chars = raw.chars().peekable();
    let mut output = String::new();
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    let newline = |output: &mut String, depth| {
        output.push('\n');
        output.push_str(&"  ".repeat(depth));
    };
    while let Some(ch) = chars.next() {
        if quoted {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                output.push(ch);
            }
            '{' | '[' => {
                output.push(ch);
                let close = if ch == '{' { '}' } else { ']' };
                if chars.peek() == Some(&close) {
                    output.push(close);
                    chars.next();
                } else {
                    depth += 1;
                    newline(&mut output, depth);
                }
            }
            '}' | ']' => {
                depth -= 1;
                newline(&mut output, depth);
                output.push(ch);
            }
            ',' => {
                output.push(ch);
                newline(&mut output, depth);
            }
            ':' => output.push_str(": "),
            _ => output.push(ch),
        }
    }
    Ok(output)
}
