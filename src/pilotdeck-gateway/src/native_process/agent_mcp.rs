//! SDK child endpoints are resolved once at SDK admission and owned by the
//! actual child session. They never mutate the parent's MCP registry.
use crate::runtime::{
    mcp::protocol::{McpServerSpec, McpTransportSpec},
    sdk_session::SdkSessionError,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn invalid(message: impl Into<String>) -> SdkSessionError {
    SdkSessionError::new("INVALID_MCP_SERVER", message.into())
}

/// Same asymmetric duplicate semantics as resolveSdkSubagentMcpServers:
/// references overwrite, while an inline entry rejects an existing name.
pub(super) fn resolve(
    agent: &JsString,
    specs: &JsValue,
    session: &[(JsString, JsValue)],
) -> Result<JsObject, SdkSessionError> {
    let entries = match specs {
        JsValue::Array(items) => items.as_slice(),
        _ => std::slice::from_ref(specs),
    };
    let mut resolved = JsObject::new();
    for spec in entries {
        if let Some(name) = spec.as_string() {
            let configured = session.iter().find(|(key,_)| key == name).map(|(_,v)|v).ok_or_else(||SdkSessionError::new("SDK_AGENT_MCP_REFERENCE_NOT_FOUND",format!("Agent {} references SDK-session MCP server {}, but it is not configured for this session.",text(agent),text(name))))?;
            if !crate::runtime::mcp::sdk::js_truthy(
                configured.get("enabled").unwrap_or(&JsValue::Undefined),
            ) {
                return Err(SdkSessionError::new(
                    "SDK_AGENT_MCP_REFERENCE_DISABLED",
                    format!(
                        "Agent {} references SDK-session MCP server {}, but it is disabled.",
                        text(agent),
                        text(name)
                    ),
                ));
            }
            resolved.insert(
                name.clone(),
                configured
                    .get("config")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
        } else {
            let entries = spec.as_object().ok_or_else(|| {
                invalid("Agent MCP servers must be references or server objects.")
            })?;
            for (name, config) in entries.own_entries() {
                if resolved
                    .get_key(name)
                    .is_some_and(crate::runtime::mcp::sdk::js_truthy)
                {
                    return Err(invalid(format!(
                        "Agent {} repeats MCP server {}.",
                        text(agent),
                        text(name)
                    )));
                }
                resolved.insert(name.clone(), config.clone());
            }
        }
    }
    Ok(resolved)
}

/// Validation does not start endpoints or resolve session references.
pub(super) fn validate(specs: &JsValue) -> Result<(), SdkSessionError> {
    let entries = match specs {
        JsValue::Object(_) => std::slice::from_ref(specs),
        JsValue::Array(items) => items.as_slice(),
        _ => return Err(invalid("Agent mcpServers must be an object or an array.")),
    };
    for spec in entries {
        if matches!(spec, JsValue::String(_)) {
            continue;
        }
        let servers = spec
            .as_object()
            .ok_or_else(|| invalid("Agent MCP server entries must be references or objects."))?;
        server_specs(servers)?;
    }
    Ok(())
}

/// Definition-local MCP uses literal SDK configuration, as
/// SubAgentSession.toSubagentMcpServerSpec does. No plugin env expansion or
/// shared-project endpoint substitution is performed here.
pub(super) fn server_specs(servers: &JsObject) -> Result<Vec<McpServerSpec>, SdkSessionError> {
    servers
        .own_entries()
        .into_iter()
        .map(|(id, config)| {
            let required = |key: &str| {
                config
                    .get(key)
                    .and_then(JsValue::as_string)
                    .map(text)
                    .ok_or_else(|| {
                        invalid(format!("MCP server {} requires string {key}.", text(id)))
                    })
            };
            let optional = |key: &str| -> Result<Option<String>, SdkSessionError> {
                match config.get(key) {
                    None | Some(JsValue::Undefined) => Ok(None),
                    Some(JsValue::String(v)) => Ok(Some(text(v))),
                    _ => Err(invalid(format!(
                        "MCP server {} requires string {key}.",
                        text(id)
                    ))),
                }
            };
            let record = |key: &str| -> Result<Option<Vec<(String, String)>>, SdkSessionError> {
                let Some(value) = config.get(key).filter(|v| !v.is_undefined()) else {
                    return Ok(None);
                };
                let object = value.as_object().ok_or_else(|| {
                    invalid(format!(
                        "MCP server {} requires an object for {key}.",
                        text(id)
                    ))
                })?;
                object
                    .own_entries()
                    .into_iter()
                    .map(|(k, v)| {
                        v.as_string().map(|v| (text(k), text(v))).ok_or_else(|| {
                            invalid(format!(
                                "MCP server {} requires string values for {key}.",
                                text(id)
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Some)
            };
            let kind = required("type")?;
            let transport = match kind.as_str() {
                "stdio" => {
                    let args = match config.get("args") {
                        None | Some(JsValue::Undefined) => None,
                        Some(JsValue::Array(items)) => Some(
                            items
                                .iter()
                                .map(|v| {
                                    v.as_string()
                                        .map(text)
                                        .ok_or_else(|| invalid("MCP command args must be strings."))
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                        )
                        .filter(|items| !items.is_empty()),
                        _ => return Err(invalid("MCP command args must be an array.")),
                    };
                    McpTransportSpec::Stdio {
                        command: required("command")?,
                        args,
                        env: record("env")?,
                        cwd: optional("cwd")?.filter(|v| !v.is_empty()),
                        per_session: false,
                    }
                }
                "sse" => McpTransportSpec::Sse {
                    url: required("url")?,
                    headers: record("headers")?,
                },
                "http" | "streamable_http" => McpTransportSpec::StreamableHttp {
                    url: required("url")?,
                    headers: record("headers")?,
                },
                _ => return Err(invalid(format!("Unknown MCP transport: {kind}."))),
            };
            let call_timeout_ms = match config.get("timeout") {
                None | Some(JsValue::Undefined) => None,
                Some(JsValue::Number(value)) if value.is_finite() => Some(*value),
                _ => return Err(invalid("MCP timeout must be a finite number.")),
            };
            Ok(McpServerSpec {
                id: text(id),
                transport,
                call_timeout_ms,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_ts_resolution_matrix_and_input_preservation() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sdk-fork-mcp/oracle.json"
        )))
        .unwrap();
        let JsValue::Array(rows) = oracle.get("rows").unwrap() else {
            panic!("rows")
        };
        assert_eq!(rows.len(), 16);
        for row in rows {
            let input = row.get("specs").unwrap();
            let before = input.clone();
            let session = match row.get("sessionServers") {
                Some(JsValue::Array(entries)) => entries
                    .iter()
                    .map(|entry| {
                        let JsValue::Array(pair) = entry else {
                            panic!("pair")
                        };
                        (pair[0].as_string().unwrap().clone(), pair[1].clone())
                    })
                    .collect::<Vec<_>>(),
                _ => vec![],
            };
            let expected = row.get("output").unwrap();
            let result = resolve(&"child".into(), input, &session);
            if expected.get("ok") == Some(&JsValue::Bool(true)) {
                assert_eq!(
                    JsValue::Object(result.unwrap()),
                    *expected.get("servers").unwrap(),
                    "{:?}",
                    row.get("id")
                );
            } else {
                let error = result.unwrap_err();
                let expected = expected.get("error").unwrap();
                assert_eq!(
                    error.code,
                    expected
                        .get("code")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap()
                );
                assert_eq!(
                    error.message,
                    text(expected.get("message").unwrap().as_string().unwrap())
                );
            }
            assert_eq!(*input, before);
        }
    }
    #[test]
    fn child_specs_are_literal_and_not_shared() {
        let specs = crate::metadata::object([(
            "literal",
            crate::metadata::object([
                ("type", "stdio".into()),
                ("command", "${env:COMMAND}".into()),
                (
                    "env",
                    crate::metadata::object([("A", "${env:VALUE}".into())]),
                ),
                ("timeout", JsValue::Number(17.5)),
            ]),
        )]);
        let specs = server_specs(specs.as_object().unwrap()).unwrap();
        assert!(
            matches!(&specs[0].transport,McpTransportSpec::Stdio{command,env:Some(env),per_session:false,..} if command=="${env:COMMAND}" && env[0].1=="${env:VALUE}")
        );
        assert_eq!(specs[0].call_timeout_ms, Some(17.5));
    }
}
