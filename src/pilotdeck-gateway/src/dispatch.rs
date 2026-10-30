//! Only verified native metadata methods may terminate a request here.
use crate::metadata::{Files, Metadata, object, text_field};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};

const LOCAL_METHODS: &[&str] = &[
    "supported_agents",
    "list_projects",
    "describe_project",
    "project_files_list",
    "project_file_read",
    "model_catalog_list",
    "session_model_get",
    "native_archive_manifest",
    "native_archive_entries",
    "native_archive_artifact",
];

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
    matches!(
        text_field(request, "method").as_deref(),
        Some(
            "list_projects"
                | "project_files_list"
                | "project_file_read"
                | "model_catalog_list"
                | "session_model_get"
                | "native_archive_manifest"
                | "native_archive_entries"
                | "native_archive_artifact"
        )
    ) || (text_field(request, "method").as_deref() == Some("describe_project")
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
    route_with_environment(request, metadata, None)
}

pub(crate) fn route_with_environment<F: Files>(
    request: &JsValue,
    metadata: Option<&Metadata<F>>,
    captured_env: Option<&crate::config::Env>,
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
    if matches!(
        method.as_str(),
        "native_archive_manifest" | "native_archive_entries" | "native_archive_artifact"
    ) {
        return metadata
            .and_then(|metadata| crate::native_archive::response(request, metadata))
            .map_or(Route::Forward, Route::LocalResponse);
    }
    if method == "model_catalog_list" || method == "session_model_get" {
        let env = captured_env.cloned().unwrap_or_else(|| {
            std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect()
        });
        if method == "session_model_get" {
            if let Some(metadata) = metadata {
                return crate::session_model::response(request, metadata, &env)
                    .map_or(Route::Forward, Route::LocalResponse);
            }
            return match crate::model_catalog::native_metadata(&env) {
                Ok(metadata) => metadata
                    .and_then(|m| crate::session_model::response(request, &m, &env))
                    .map_or(Route::Forward, Route::LocalResponse),
                Err(error) => metadata_error_response(request, &error),
            };
        }
        return route_catalog(request, metadata, &env);
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
                        // Shared with the SDK MCP slice's `project_key`
                        // (#347 fix-r1): Node's Received text, with
                        // `Route::Forward` for the shapes whose text Node
                        // never produces (a primitive constructor, an
                        // unconvertible `constructor.name`).
                        let crate::metadata::ArgumentReceived::Text(received) =
                            crate::metadata::argument_received(root)
                        else {
                            return Route::Forward;
                        };
                        let Ok(message) =
                            crate::metadata::invalid_arg_type_message("paths[0]", &received)
                                .to_utf8()
                        else {
                            return Route::Forward;
                        };
                        Err(("ERR_INVALID_ARG_TYPE", message))
                    }
                }
            }
        }
        "project_files_list" | "project_file_read" => {
            let Some(metadata) = metadata else {
                return Route::Forward;
            };
            let Some(result) = crate::project_files::execute(
                metadata,
                &method,
                request.get("params").unwrap_or(&JsValue::Undefined),
            ) else {
                return Route::Forward;
            };
            result
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

pub(crate) fn error_number(number: f64) -> String {
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

pub(crate) fn route_catalog<F: Files>(
    request: &JsValue,
    metadata: Option<&Metadata<F>>,
    env: &crate::config::Env,
) -> Route {
    if !is_request(request)
        || text_field(request, "method").as_deref() != Some("model_catalog_list")
    {
        return Route::Forward;
    }
    if let Some(metadata) = metadata {
        return crate::model_catalog::response(request, metadata, env)
            .map_or(Route::Forward, Route::LocalResponse);
    }
    match crate::model_catalog::native_metadata(env) {
        Ok(Some(metadata)) => crate::model_catalog::response(request, &metadata, env)
            .map_or(Route::Forward, Route::LocalResponse),
        Ok(None) => Route::Forward,
        Err(error) => metadata_error_response(request, &error),
    }
}

fn metadata_error_response(request: &JsValue, error: &crate::config::Error) -> Route {
    let response = object([
        ("type", "response".into()),
        (
            "id",
            request.get("id").expect("validated request id").clone(),
        ),
        ("ok", JsValue::Bool(false)),
        (
            "error",
            object([
                ("code", JsValue::String(error.code.as_str().into())),
                ("message", JsValue::String(error.message.clone())),
            ]),
        ),
    ]);
    Route::LocalResponse(stringify_json(&response).unwrap().unwrap().into_bytes())
}
