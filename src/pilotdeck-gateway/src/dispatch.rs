//! Only verified native metadata methods may terminate a request here.
use crate::metadata::{Files, Metadata, object, string, text_field};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};

const LOCAL_METHODS: &[&str] = &["supported_agents", "list_projects", "describe_project"];

pub(crate) enum Route {
    Forward,
    LocalResponse(Vec<u8>),
}

pub(crate) fn parse(payload: &[u8]) -> Option<JsValue> {
    // Node Buffer.toString("utf8") replaces malformed bytes before JSON.parse.
    parse_json(&String::from_utf8_lossy(payload)).ok()
}

pub(crate) fn is_hello(frame: &JsValue) -> bool {
    text_field(frame, "type").as_deref() == Some("hello")
        && ["protocolVersion", "clientName", "clientVersion", "token"]
            .iter()
            .all(|key| frame.get(key).and_then(JsValue::as_string).is_some())
}

pub(crate) fn is_request(frame: &JsValue) -> bool {
    text_field(frame, "type").as_deref() == Some("request")
        && frame.get("id").and_then(JsValue::as_string).is_some()
        && frame.get("method").and_then(JsValue::as_string).is_some()
}

pub(crate) fn is_local(request: &JsValue) -> bool {
    text_field(request, "method").is_some_and(|method| LOCAL_METHODS.contains(&method.as_str()))
}

pub(crate) fn needs_files(request: &JsValue) -> bool {
    text_field(request, "method").as_deref() == Some("list_projects")
        || (text_field(request, "method").as_deref() == Some("describe_project")
            && request
                .get("params")
                .and_then(|params| params.get("projectKey"))
                .and_then(JsValue::as_string)
                .is_some())
}

pub(crate) fn route_with_metadata<F: Files>(
    request: &JsValue,
    metadata: Option<&Metadata<F>>,
) -> Route {
    if !is_request(request) {
        return Route::Forward;
    }
    let Some(method) = text_field(request, "method") else {
        return Route::Forward;
    };
    if !LOCAL_METHODS.contains(&method.as_str()) {
        return Route::Forward;
    }
    let result = match method.as_str() {
        "supported_agents" => Ok(supported_agents()),
        "list_projects" => match metadata {
            Some(metadata) => Ok(metadata.list_projects()),
            None => return Route::Forward,
        },
        "describe_project" => {
            let Some(metadata) = metadata else {
                return Route::Forward;
            };
            let undefined = JsValue::Undefined;
            let params = request.get("params").unwrap_or(&undefined);
            if matches!(params, JsValue::Undefined | JsValue::Null) {
                let kind = if params.is_undefined() {
                    "undefined"
                } else {
                    "null"
                };
                Err((
                    "gateway_request_failed",
                    format!("Cannot read properties of {kind} (reading 'projectKey')"),
                ))
            } else {
                let root = params.get("projectKey").unwrap_or(&undefined);
                match root.as_string() {
                    Some(root) => Ok(metadata.describe_project(root)),
                    None => {
                        let received = match root {
                            JsValue::Undefined => "undefined".into(),
                            JsValue::Null => "null".into(),
                            JsValue::Number(number) => {
                                format!("type number ({})", error_number(*number))
                            }
                            JsValue::Bool(value) => format!("type boolean ({value})"),
                            JsValue::Array(_) => "an instance of Array".into(),
                            JsValue::Object(object) => match object.get("constructor") {
                                None => "an instance of Object".into(),
                                Some(constructor) => match constructor.get("name") {
                                    Some(name)
                                        if name
                                            .as_string()
                                            .is_some_and(|name| !name.is_empty()) =>
                                    {
                                        match string(name) {
                                            Some(name) => format!("an instance of {name}"),
                                            None => return Route::Forward,
                                        }
                                    }
                                    Some(JsValue::Object(_) | JsValue::Array(_)) => {
                                        return Route::Forward;
                                    }
                                    Some(JsValue::Number(number))
                                        if *number != 0.0 && !number.is_nan() =>
                                    {
                                        format!("an instance of {}", error_number(*number))
                                    }
                                    Some(JsValue::Bool(true)) => "an instance of true".into(),
                                    _ => "[Object]".into(),
                                },
                            },
                            // Node util.inspect has a separate contract. Unsupported
                            // invalid argument shapes remain TS-owned.
                            _ => return Route::Forward,
                        };
                        Err((
                            "ERR_INVALID_ARG_TYPE",
                            format!(
                                "The \"paths[0]\" argument must be of type string. Received {received}"
                            ),
                        ))
                    }
                }
            }
        }
        _ => unreachable!("checked whitelist"),
    };
    let id = request.get("id").expect("validated request id").clone();
    let response = match result {
        Ok(result) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(true)),
            ("result", result),
        ]),
        Err((code, message)) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(false)),
            (
                "error",
                object([("code", code.into()), ("message", message.as_str().into())]),
            ),
        ]),
    };
    match stringify_json(&response).ok().flatten() {
        Some(response) => Route::LocalResponse(response.into_bytes()),
        None => Route::Forward,
    }
}

fn error_number(number: f64) -> String {
    if number.is_nan() {
        "NaN".into()
    } else if number == f64::INFINITY {
        "Infinity".into()
    } else if number == f64::NEG_INFINITY {
        "-Infinity".into()
    } else if number == 0.0 && number.is_sign_negative() {
        "-0".into()
    } else {
        stringify_json(&JsValue::Number(number))
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}

fn supported_agents() -> JsValue {
    // Protocol data, independently expressed from the fixed TS preset projection.
    let definitions: &[(&str, &str, &[&str], bool)] = &[
        (
            "general-purpose",
            "General-purpose subagent for complex research/synthesis tasks. Has broad parent-tool access; nested delegation follows the configured depth cap.",
            &["*"],
            false,
        ),
        (
            "explore",
            "Read-only exploration subagent. Inspects files, runs grep/glob, and may run safe shell commands. Cannot edit files.",
            &["read_file", "grep", "glob", "bash"],
            true,
        ),
        (
            "plan",
            "Read-only planning subagent. Inspects code via read/grep/glob and produces a step-by-step plan.",
            &["read_file", "grep", "glob"],
            true,
        ),
        (
            "verify",
            "Verification subagent. Inspects generated artifacts (images, HTML, PDFs) for correctness. Can read files, run shell commands, and search code, but cannot modify files.",
            &["read_file", "grep", "glob", "bash"],
            true,
        ),
    ];
    object([(
        "agents",
        JsValue::Array(
            definitions
                .iter()
                .map(|(name, description, tools, read_only)| {
                    object([
                        ("name", (*name).into()),
                        ("description", (*description).into()),
                        (
                            "tools",
                            JsValue::Array(tools.iter().map(|tool| (*tool).into()).collect()),
                        ),
                        ("readOnly", JsValue::Bool(*read_only)),
                    ])
                })
                .collect(),
        ),
    )])
}

#[cfg(test)]
pub(crate) mod tests;
