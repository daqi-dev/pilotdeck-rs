//! Default prompt assembly and original message projection, with explicit
//! environment facts and no extension, memory or instruction-discovery ports.
use crate::{
    JsObject, JsValue,
    projection::{MaxMessages, ProjectionError, project},
    prompt,
};
pub struct Environment<'a> {
    pub platform: &'a str,
    pub node_identity: &'a str,
    pub date: &'a str,
}
fn owned_text(fields: &JsObject, key: &str) -> Result<String, ProjectionError> {
    fields
        .get(key)
        .and_then(JsValue::as_string)
        .ok_or_else(|| ProjectionError::new(format!("{key} must be text")))?
        .to_utf8()
        .map_err(|_| ProjectionError::new("native prompt environment requires valid UTF-8"))
}
fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}
pub fn prepare(
    fields: &JsObject,
    environment: Environment<'_>,
) -> Result<JsValue, ProjectionError> {
    for key in [
        "instructionLayers",
        "memoryContext",
        "promptContributions",
        "extensions",
    ] {
        if fields
            .get(key)
            .is_some_and(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
        {
            return Err(ProjectionError::new(format!(
                "{key} requires an unconfigured context port"
            )));
        }
    }
    if fields.get("supportsPromptCache") == Some(&JsValue::Bool(true)) {
        return Err(ProjectionError::new(
            "prompt cache coordinator is not configured",
        ));
    }
    let messages = match fields.get("messages") {
        Some(JsValue::Array(messages)) => messages
            .iter()
            .filter(|m| {
                m.get("metadata")
                    .and_then(|m| m.get("purpose"))
                    .and_then(JsValue::as_string)
                    .is_none_or(|s| !s.equals_str("runtime_context"))
            })
            .cloned()
            .collect::<Vec<_>>(),
        _ => return Err(ProjectionError::new("messages array required")),
    };
    let tools = match fields.get("tools") {
        Some(JsValue::Array(tools)) => tools,
        _ => return Err(ProjectionError::new("tools array required")),
    };
    let maximum = match fields.get("maxMessages") {
        None | Some(JsValue::Undefined) => None,
        Some(JsValue::Number(n)) => Some(MaxMessages {
            value: *n,
            raw: pilotdeck_protocol::number::number_to_string(*n),
        }),
        Some(JsValue::Null) => Some(MaxMessages {
            value: 0.0,
            raw: "null".into(),
        }),
        _ => {
            return Err(ProjectionError::new(
                "maxMessages requires numeric wire value",
            ));
        }
    };
    let projected = project(&messages, maximum.as_ref())?;
    let dirs = match fields.get("additionalWorkingDirectories") {
        Some(JsValue::Array(d)) => d
            .iter()
            .map(|v| {
                v.as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .ok_or_else(|| ProjectionError::new("working directories must be text"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![],
        _ => return Err(ProjectionError::new("working directories must be array")),
    };
    let cwd = owned_text(fields, "cwd")?;
    let provider = owned_text(fields, "provider")?;
    let model = owned_text(fields, "model")?;
    let mode = owned_text(fields, "permissionMode")?;
    let run = fields
        .get("runMode")
        .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
        .map(|v| {
            v.as_string()
                .and_then(|s| s.to_utf8().ok())
                .ok_or_else(|| ProjectionError::new("runMode must be text"))
        })
        .transpose()?;
    let custom = fields.get("customSystemPrompt");
    let use_custom = custom.is_some_and(|v| !v.is_undefined());
    let mut parts = if use_custom {
        match custom {
            Some(JsValue::String(s)) => {
                let value = s.to_utf8().map_err(|_| {
                    ProjectionError::new("customSystemPrompt contains invalid UTF-16")
                })?;
                if trim(&value).is_empty() {
                    vec![]
                } else {
                    vec![trim(&value).to_owned()]
                }
            }
            Some(JsValue::Null) => vec![],
            _ => {
                return Err(ProjectionError::new(
                    "customSystemPrompt must be text or null",
                ));
            }
        }
    } else {
        prompt::build_default_system_prompt(&mode, run.as_deref(), &dirs, tools, "")
            .map_err(|e| ProjectionError::new(e.message))?
    };
    let user = prompt::build_user_context(
        &cwd,
        &provider,
        &model,
        &mode,
        run.as_deref(),
        environment.platform,
        environment.node_identity,
    );
    let surface = fields
        .get("runtimeContextSurface")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok());
    let mut messages = projected.messages;
    if matches!(surface.as_deref(), None | Some("system_prompt")) {
        parts.extend(user.clone());
    }
    if !use_custom {
        parts.push(format!(
            "<environment>\nnow: {}\n</environment>",
            environment.date
        ));
    }
    if let Some(value) = fields
        .get("appendSystemPrompt")
        .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
    {
        let value = value
            .as_string()
            .and_then(|s| s.to_utf8().ok())
            .ok_or_else(|| ProjectionError::new("appendSystemPrompt must be text"))?;
        if !trim(&value).is_empty() {
            parts.push(trim(&value).to_owned());
        }
    }
    let mut runtime_messages = vec![];
    if surface.as_deref() == Some("user_message") {
        let context=crate::object([("role","user".into()),("content",JsValue::Array(vec![crate::text_block(format!("<runtime-context name=\"pilotdeck:user-context:0\">\n{}\n</runtime-context>",user.join("\n\n")).into())])),("metadata",crate::object([("synthetic",JsValue::Bool(true)),("purpose","runtime_context".into())]))]);
        let insertion = messages
            .iter()
            .rposition(crate::is_real_user_request_message)
            .unwrap_or(messages.len());
        messages.insert(insertion, context.clone());
        runtime_messages.push(context);
    }
    let diagnostics = projected
        .warnings
        .into_iter()
        .map(|w| {
            crate::object([
                ("code", w.code.into()),
                ("severity", "warning".into()),
                ("message", JsValue::String(w.message.into())),
            ])
        })
        .collect();
    let mut materialization = JsObject::new();
    materialization.insert(
        "runtimeContexts".into(),
        JsValue::Array(vec![crate::object([
            ("name", "pilotdeck:user-context:0".into()),
            ("text", JsValue::String(user.join("\n\n").into())),
        ])]),
    );
    if !runtime_messages.is_empty() {
        materialization.insert(
            "runtimeContextMessages".into(),
            JsValue::Array(runtime_messages),
        );
    }
    Ok(crate::object([
        ("messages", JsValue::Array(messages)),
        ("tools", JsValue::Array(tools.clone())),
        ("systemPrompt", JsValue::String(parts.join("\n\n").into())),
        (
            "systemPromptParts",
            JsValue::Array(
                parts
                    .into_iter()
                    .map(|s| JsValue::String(s.into()))
                    .collect(),
            ),
        ),
        ("diagnostics", JsValue::Array(diagnostics)),
        ("boundaries", JsValue::Array(vec![])),
        (
            "metadata",
            crate::object([
                ("droppedCount", JsValue::Number(projected.dropped_count)),
                ("toolCount", JsValue::Number(tools.len() as f64)),
            ]),
        ),
        ("materialization", JsValue::Object(materialization)),
    ]))
}
