//! Fresh pure projection of the pinned original default factory. No ambient IO,
//! host calls or seed/model-state admission. Coordinator owns trait integration.
use crate::AdvertisedMethods;
use crate::execute::{
    FactoryModelOverride, FactoryPayload, FactoryPayloadError, FactoryPermissionContext,
    FactoryPermissionRules, FactoryRuntimeConfig, FactorySubagentModel, FactoryTurnInput, RunMode,
};
use pilotdeck_agent::ports::{PermissionMode, Route, RuntimeContextSurface, ToolDescriptor};
use pilotdeck_protocol::model::{BoundarySnapshot, ThinkingConfig, ThinkingMode, ToolChoice};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

fn record(value: Option<&JsValue>) -> Option<&JsObject> {
    value.and_then(JsValue::as_object)
}
fn get<'a>(object: Option<&'a JsObject>, key: &str) -> Option<&'a JsValue> {
    object.and_then(|o| o.get(key))
}
fn nullish(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined | JsValue::Null))
}
fn or<'a>(a: Option<&'a JsValue>, b: Option<&'a JsValue>) -> Option<&'a JsValue> {
    if nullish(a) { b } else { a }
}
fn string(value: Option<&JsValue>) -> Option<JsString> {
    value.and_then(JsValue::as_string).cloned()
}
fn nonempty(value: Option<&JsValue>) -> Option<JsString> {
    string(value).filter(|s| !s.is_empty())
}
fn boolean(value: Option<&JsValue>) -> Option<bool> {
    value.and_then(JsValue::as_bool)
}
fn is(value: Option<&JsValue>, text: &str) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(text))
}
fn integer(value: Option<&JsValue>) -> Option<f64> {
    value
        .and_then(JsValue::as_number)
        .filter(|n| n.is_finite() && n.fract() == 0.0)
}
fn positive_integer(value: Option<&JsValue>) -> Option<f64> {
    integer(value).filter(|n| *n > 0.0)
}
fn positive(value: Option<&JsValue>) -> Option<f64> {
    value
        .and_then(JsValue::as_number)
        .filter(|n| n.is_finite() && *n > 0.0)
}
fn nonnegative(value: Option<&JsValue>) -> Option<f64> {
    value
        .and_then(JsValue::as_number)
        .filter(|n| n.is_finite() && *n >= 0.0)
}
fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut o = JsObject::new();
    for (key, value) in fields {
        o.insert(key.into(), value);
    }
    o
}
fn required_string(value: Option<&JsValue>) -> Result<JsString, FactoryPayloadError> {
    enum Part<'a> {
        Value(Option<&'a JsValue>, bool),
        Comma,
    }
    let mut pending = vec![Part::Value(value, false)];
    let mut out = Vec::new();
    while let Some(part) = pending.pop() {
        let (v, in_array) = match part {
            Part::Comma => {
                out.push(u16::from(b','));
                continue;
            }
            Part::Value(v, a) => (v, a),
        };
        let s: JsString = match v {
            None | Some(JsValue::Undefined | JsValue::Null) if in_array => continue,
            None | Some(JsValue::Undefined) => "undefined".into(),
            Some(JsValue::Null) => "null".into(),
            Some(JsValue::String(s)) => s.clone(),
            Some(JsValue::Bool(b)) => if *b { "true" } else { "false" }.into(),
            Some(JsValue::Number(n)) if n.is_nan() => "NaN".into(),
            Some(JsValue::Number(n)) if *n == f64::INFINITY => "Infinity".into(),
            Some(JsValue::Number(n)) if *n == f64::NEG_INFINITY => "-Infinity".into(),
            Some(v @ JsValue::Number(_)) => stringify_json(v)
                .map_err(|_| FactoryPayloadError::SourceTypeError {
                    message: "Number String conversion failed.".into(),
                })?
                .ok_or_else(|| FactoryPayloadError::SourceTypeError {
                    message: "Number String conversion returned undefined.".into(),
                })?
                .into(),
            Some(JsValue::Array(a)) => {
                for (i, v) in a.iter().enumerate().rev() {
                    pending.push(Part::Value(Some(v), true));
                    if i > 0 {
                        pending.push(Part::Comma);
                    }
                }
                continue;
            }
            Some(JsValue::Object(o)) if !o.contains_key("toString") => "[object Object]".into(),
            Some(JsValue::Object(_)) => {
                return Err(FactoryPayloadError::SourceTypeError {
                    message: "Cannot convert object to primitive value".into(),
                });
            }
        };
        out.extend_from_slice(s.units());
    }
    Ok(JsString::from_utf16(out))
}
fn permission(value: Option<&JsValue>) -> Option<PermissionMode> {
    if is(value, "default") {
        Some(PermissionMode::Default)
    } else if is(value, "plan") {
        Some(PermissionMode::Plan)
    } else if is(value, "bypassPermissions") {
        Some(PermissionMode::BypassPermissions)
    } else {
        None
    }
}
fn run_mode(value: Option<&JsValue>) -> Option<RunMode> {
    if is(value, "agent") {
        Some(RunMode::Agent)
    } else if is(value, "plan") {
        Some(RunMode::Plan)
    } else if is(value, "ask") {
        Some(RunMode::Ask)
    } else {
        None
    }
}
fn thinking(value: Option<&JsValue>) -> Option<ThinkingConfig> {
    let o = record(value)?;
    let enabled = boolean(o.get("enabled"))?;
    let mode = [
        ("default", ThinkingMode::Default),
        ("off", ThinkingMode::Off),
        ("minimal", ThinkingMode::Minimal),
        ("low", ThinkingMode::Low),
        ("medium", ThinkingMode::Medium),
        ("high", ThinkingMode::High),
        ("xhigh", ThinkingMode::XHigh),
        ("max", ThinkingMode::Max),
    ]
    .into_iter()
    .find_map(|(name, mode)| is(o.get("mode"), name).then_some(mode));
    Some(ThinkingConfig {
        enabled,
        mode,
        budget_tokens: positive_integer(o.get("budgetTokens")),
        preserve: boolean(o.get("preserve")),
        split_reasoning: boolean(o.get("splitReasoning")),
    })
}
fn tool_choice(value: Option<&JsValue>) -> Option<ToolChoice> {
    if is(value, "auto") {
        return Some(ToolChoice::Auto);
    }
    if is(value, "none") {
        return Some(ToolChoice::None);
    }
    if is(value, "required") {
        return Some(ToolChoice::Required);
    }
    let o = record(value)?;
    if is(o.get("type"), "tool") {
        Some(ToolChoice::Tool {
            name: nonempty(o.get("name"))?,
        })
    } else {
        None
    }
}
fn subagent(value: Option<&JsValue>) -> Result<Option<FactorySubagentModel>, FactoryPayloadError> {
    if matches!(value, None | Some(JsValue::Undefined)) {
        return Ok(None);
    }
    let err = |text: &str| FactoryPayloadError::InvalidSubagentModel {
        message: text.into(),
    };
    let o = record(value).ok_or_else(|| err("agent.subagentModel must be an object."))?;
    let provider = nonempty(o.get("provider"))
        .ok_or_else(|| err("agent.subagentModel.provider must be a non-empty string."))?;
    let model = nonempty(o.get("model"))
        .ok_or_else(|| err("agent.subagentModel.model must be a non-empty string."))?;
    let required = |key: &str| -> Result<Option<f64>, FactoryPayloadError> {
        let value = o.get(key);
        if matches!(value, None | Some(JsValue::Undefined)) {
            return Ok(None);
        }
        positive_integer(value).map(Some).ok_or_else(|| {
            err(&format!(
                "agent.subagentModel.{key} must be a positive integer."
            ))
        })
    };
    Ok(Some(FactorySubagentModel {
        route: Route { provider, model },
        max_context_tokens: required("maxContextTokens")?,
        max_output_tokens: required("maxOutputTokens")?,
    }))
}
fn model_override(value: Option<&JsValue>) -> Option<FactoryModelOverride> {
    let o = record(value)?;
    Some(FactoryModelOverride {
        route: Route {
            provider: nonempty(o.get("provider"))?,
            model: nonempty(o.get("model"))?,
        },
        temperature: o
            .get("temperature")
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite()),
        speed: o
            .get("speed")
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite()),
        thinking: thinking(o.get("thinking")),
    })
}
fn rules(value: Option<&JsValue>) -> Option<FactoryPermissionRules> {
    let o = record(value)?;
    let array = |key| match o.get(key) {
        Some(JsValue::Array(a)) => Some(a.clone()),
        _ => None,
    };
    Some(FactoryPermissionRules {
        allow: array("allow"),
        deny: array("deny"),
        ask: array("ask"),
    })
}
fn directories(value: Option<&JsValue>) -> Vec<JsString> {
    if let Some(JsValue::Array(a)) = value {
        a.iter()
            .map(|v| v.as_string().cloned())
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    } else {
        vec![]
    }
}
fn merge_metadata(
    sources: [Option<&JsObject>; 3],
) -> Result<Option<JsObject>, FactoryPayloadError> {
    let mut merged = None;
    for source in sources.into_iter().flatten() {
        let target = merged.get_or_insert_with(JsObject::new);
        for (key, value) in source.entries() {
            if key.equals_str("__proto__") {
                if matches!(
                    value,
                    JsValue::Null | JsValue::Object(_) | JsValue::Array(_)
                ) {
                    return Err(FactoryPayloadError::NativeGraphRequired{message:"Default factory metadata Object.assign __proto__ requires native prototype projection.".into()});
                }
                // The ordinary Object.prototype setter ignores primitive values
                // and creates no own property. Actual graph changes remain open.
                continue;
            }
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(merged)
}
fn methods(host: Option<&JsObject>) -> AdvertisedMethods {
    let supported: [(&str, &[&str]); 8] = [
        (
            "model",
            &[
                "prepare",
                "materialize_prepared_request",
                "stream",
                "stream_next",
                "close_stream",
                "get_metadata",
            ],
        ),
        (
            "budget",
            &[
                "estimate_request_input",
                "evaluate_request_budget",
                "estimate_usage_cost",
            ],
        ),
        (
            "turn",
            &["drain_steer", "drain_or_close_steer", "persist_compaction"],
        ),
        (
            "context",
            &[
                "prepare_for_model",
                "apply_tool_results",
                "recover_from_model_error",
                "capture_turn",
                "try_auto_compact",
            ],
        ),
        ("lifecycle", &["dispatch"]),
        ("event", &["emit"]),
        (
            "capability",
            &["execute", "execute_batch", "list_tools", "plan_todo"],
        ),
        ("permission", &["decide"]),
    ];
    AdvertisedMethods {
        modules: supported
            .into_iter()
            .map(|(module, names)| {
                let source = record(get(host, module));
                let list = match get(source, "methods") {
                    Some(JsValue::Array(a)) => a
                        .iter()
                        .filter_map(|v| v.as_string())
                        .filter(|s| names.iter().any(|n| s.equals_str(n)))
                        .cloned()
                        .collect(),
                    _ => vec![],
                };
                (module.into(), list)
            })
            .collect(),
    }
}
fn descriptors(value: Option<&JsValue>) -> Vec<ToolDescriptor> {
    let values = match value {
        Some(JsValue::Array(a)) => Some(a),
        _ => match get(record(value), "available") {
            Some(JsValue::Array(a)) => Some(a),
            _ => None,
        },
    };
    let mut out = vec![];
    for value in values.into_iter().flatten() {
        let Some(o) = value.as_object() else {
            continue;
        };
        let Some(name) = nonempty(o.get("name")) else {
            continue;
        };
        let kind = string(o.get("kind"))
            .filter(|s| {
                [
                    "filesystem",
                    "shell",
                    "network",
                    "mcp",
                    "session",
                    "agent",
                    "structured_output",
                    "custom",
                ]
                .iter()
                .any(|k| s.equals_str(k))
            })
            .unwrap_or_else(|| "custom".into());
        let interaction = boolean(o.get("requiresUserInteraction")) == Some(true);
        let mut caps = vec![];
        if let Some(JsValue::Array(a)) = o.get("requiredRuntimeCapabilities") {
            for capability in a.iter().filter_map(JsValue::as_string) {
                if [
                    "always_on_run_context",
                    "plan_workflow",
                    "subagent_fork",
                    "user_interaction",
                ]
                .iter()
                .any(|c| capability.equals_str(c))
                    && !caps.contains(capability)
                {
                    caps.push(capability.clone());
                }
            }
        }
        for (needed, name) in [
            (kind.equals_str("agent"), "subagent_fork"),
            (interaction, "user_interaction"),
        ] {
            if needed && !caps.iter().any(|c| c.equals_str(name)) {
                caps.push(name.into());
            }
        }
        let schema = record(or(o.get("inputSchema"), o.get("input_schema")))
            .filter(|o| is(o.get("type"), "object"))
            .cloned()
            .unwrap_or_else(|| object([("type", "object".into())]));
        out.push(ToolDescriptor {
            name,
            description: nonempty(o.get("description")).unwrap_or_else(|| "Host capability".into()),
            kind,
            input_schema: schema,
            read_only: boolean(o.get("readOnly")) == Some(true),
            concurrency_safe: boolean(o.get("concurrencySafe")) == Some(true),
            requires_user_interaction: interaction,
            required_runtime_capabilities: Some(caps),
        });
    }
    out
}
fn copy_string(out: &mut JsObject, source: &JsObject, key: &'static str) {
    if let Some(value) = string(source.get(key)) {
        out.insert(key.into(), JsValue::String(value));
    }
}
fn copy_boolean(out: &mut JsObject, source: &JsObject, key: &'static str) {
    if let Some(value) = boolean(source.get(key)) {
        out.insert(key.into(), JsValue::Bool(value));
    }
}
fn timeline(value: Option<&JsValue>) -> Option<JsValue> {
    let o = record(value)?;
    if o.get("version").and_then(JsValue::as_number) != Some(1.0) {
        return None;
    }
    let mut out = object([
        ("version", JsValue::Number(1.0)),
        ("turnId", JsValue::String(string(o.get("turnId"))?)),
        ("id", JsValue::String(string(o.get("id"))?)),
    ]);
    copy_string(&mut out, o, "previousId");
    out.insert("order".into(), JsValue::Number(integer(o.get("order"))?));
    out.insert(
        "revision".into(),
        JsValue::Number(integer(o.get("revision"))?),
    );
    if let Some(n) = integer(o.get("offset")) {
        out.insert("offset".into(), JsValue::Number(n));
    }
    Some(JsValue::Object(out))
}
fn annotate(out: &mut JsObject, source: &JsObject) {
    if let Some(value) = timeline(source.get("timeline")) {
        out.insert("timeline".into(), value);
    }
}
fn text_block(text: JsString) -> JsValue {
    JsValue::Object(object([
        ("type", "text".into()),
        ("text", JsValue::String(text)),
    ]))
}
fn data_url(url: &JsString) -> Option<(JsString, JsString)> {
    let units = url.units();
    let prefix: Vec<_> = "data:".encode_utf16().collect();
    if !units.starts_with(&prefix) {
        return None;
    }
    let semicolon = units[5..].iter().position(|u| *u == u16::from(b';'))? + 5;
    if semicolon == 5 {
        return None;
    }
    let marker: Vec<_> = ";base64,".encode_utf16().collect();
    if !units[semicolon..].starts_with(&marker) {
        return None;
    }
    let data = &units[semicolon + marker.len()..];
    if data.is_empty()
        || data
            .iter()
            .any(|u| matches!(*u, 0x000a | 0x000d | 0x2028 | 0x2029))
    {
        return None;
    }
    Some((
        JsString::from_utf16(units[5..semicolon].to_vec()),
        JsString::from_utf16(data.to_vec()),
    ))
}
fn leaf(o: &JsObject) -> Option<JsValue> {
    let mut out;
    if is(o.get("type"), "text") || is(o.get("type"), "thinking") {
        let kind = if is(o.get("type"), "text") {
            "text"
        } else {
            "thinking"
        };
        out = object([
            ("type", kind.into()),
            ("text", JsValue::String(string(o.get("text"))?)),
        ]);
        copy_string(&mut out, o, "blockId");
        if kind == "thinking" {
            copy_string(&mut out, o, "signature");
            copy_string(&mut out, o, "reasoningContent");
        }
    } else if is(o.get("type"), "tool_call") {
        out = object([
            ("type", "tool_call".into()),
            ("id", JsValue::String(string(o.get("id"))?)),
            ("name", JsValue::String(string(o.get("name"))?)),
            (
                "input",
                if nullish(o.get("input")) {
                    JsValue::Object(JsObject::new())
                } else {
                    o.get("input")?.clone()
                },
            ),
        ]);
        if let Some(v) = o.get("raw").filter(|v| !v.is_undefined()) {
            out.insert("raw".into(), v.clone());
        }
    } else if is(o.get("type"), "image")
        && (is(o.get("source"), "base64") || is(o.get("source"), "url"))
    {
        out = object([
            ("type", "image".into()),
            ("source", JsValue::String(string(o.get("source"))?)),
            ("mimeType", JsValue::String(string(o.get("mimeType"))?)),
            ("data", JsValue::String(string(o.get("data"))?)),
        ]);
        if let Some(n) = nonnegative(o.get("bytes")) {
            out.insert("bytes".into(), JsValue::Number(n));
        }
        if ["auto", "low", "high"]
            .iter()
            .any(|k| is(o.get("detail"), k))
        {
            out.insert("detail".into(), o.get("detail")?.clone());
        }
    } else if is(o.get("type"), "pdf")
        && is(o.get("source"), "base64")
        && is(o.get("mimeType"), "application/pdf")
    {
        out = object([
            ("type", "pdf".into()),
            ("source", "base64".into()),
            ("data", JsValue::String(string(o.get("data"))?)),
            ("mimeType", "application/pdf".into()),
            ("bytes", JsValue::Number(nonnegative(o.get("bytes"))?)),
        ]);
        if let Some(n) = positive_integer(o.get("pages")) {
            out.insert("pages".into(), JsValue::Number(n));
        }
    } else if is(o.get("type"), "audio")
        && (is(o.get("source"), "base64") || is(o.get("source"), "url"))
    {
        out = object([
            ("type", "audio".into()),
            ("source", JsValue::String(string(o.get("source"))?)),
            ("data", JsValue::String(string(o.get("data"))?)),
            ("mimeType", JsValue::String(string(o.get("mimeType"))?)),
        ]);
        for key in ["bytes", "durationSeconds"] {
            if let Some(n) = nonnegative(o.get(key)) {
                out.insert(key.into(), JsValue::Number(n));
            }
        }
    } else if is(o.get("type"), "tool_result_reference") {
        out = object([
            ("type", "tool_result_reference".into()),
            ("toolCallId", JsValue::String(string(o.get("toolCallId"))?)),
            ("path", JsValue::String(string(o.get("path"))?)),
            (
                "originalBytes",
                JsValue::Number(nonnegative(o.get("originalBytes"))?),
            ),
            ("preview", JsValue::String(string(o.get("preview"))?)),
            ("hasMore", JsValue::Bool(boolean(o.get("hasMore"))?)),
        ]);
        if boolean(o.get("isError")) == Some(true) {
            out.insert("isError".into(), JsValue::Bool(true));
        }
        for key in ["readFilePath", "mimeType", "reason"] {
            copy_string(&mut out, o, key);
        }
    } else if is(o.get("type"), "media_reference")
        && ["image", "pdf", "audio"]
            .iter()
            .any(|m| is(o.get("mediaType"), m))
    {
        out = object([
            ("type", "media_reference".into()),
            ("path", JsValue::String(string(o.get("path"))?)),
            (
                "originalBytes",
                JsValue::Number(nonnegative(o.get("originalBytes"))?),
            ),
            ("preview", JsValue::String(string(o.get("preview"))?)),
            ("hasMore", JsValue::Bool(boolean(o.get("hasMore"))?)),
            ("mimeType", JsValue::String(string(o.get("mimeType"))?)),
            ("mediaType", JsValue::String(string(o.get("mediaType"))?)),
        ]);
        copy_string(&mut out, o, "toolCallId");
        if let Some(n) = positive_integer(o.get("pages")) {
            out.insert("pages".into(), JsValue::Number(n));
        }
        if ["auto", "low", "high"]
            .iter()
            .any(|k| is(o.get("detail"), k))
        {
            out.insert("detail".into(), o.get("detail")?.clone());
        }
        copy_string(&mut out, o, "reason");
    } else if is(o.get("type"), "image_url") {
        let url = string(get(record(o.get("image_url")), "url"))?;
        let (mime, data) = data_url(&url)?;
        return Some(JsValue::Object(object([
            ("type", "image".into()),
            ("source", "base64".into()),
            ("mimeType", JsValue::String(mime)),
            ("data", JsValue::String(data)),
        ])));
    } else {
        return None;
    }
    annotate(&mut out, o);
    Some(JsValue::Object(out))
}
fn blocks(values: &[JsValue]) -> Vec<JsValue> {
    enum Work<'a> {
        Visit(&'a JsValue),
        Finish(JsObject),
    }
    let mut pending: Vec<_> = values.iter().rev().map(Work::Visit).collect();
    let mut output = vec![vec![]];
    while let Some(work) = pending.pop() {
        match work {
            Work::Visit(value) => {
                let Some(o) = value.as_object() else {
                    continue;
                };
                if is(o.get("type"), "tool_result") {
                    let Some(id) = string(o.get("toolCallId")) else {
                        continue;
                    };
                    let mut out = object([
                        ("type", "tool_result".into()),
                        ("toolCallId", JsValue::String(id)),
                        ("content", JsValue::Array(vec![])),
                    ]);
                    if boolean(o.get("isError")) == Some(true) {
                        out.insert("isError".into(), JsValue::Bool(true));
                    }
                    if let Some(v) = o.get("raw").filter(|v| !v.is_undefined()) {
                        out.insert("raw".into(), v.clone());
                    }
                    annotate(&mut out, o);
                    match o.get("content") {
                        Some(JsValue::Array(a)) => {
                            pending.push(Work::Finish(out));
                            output.push(vec![]);
                            pending.extend(a.iter().rev().map(Work::Visit));
                        }
                        other => {
                            if let Some(s) = string(other) {
                                out.insert("content".into(), JsValue::Array(vec![text_block(s)]));
                            }
                            output
                                .last_mut()
                                .expect("root output")
                                .push(JsValue::Object(out));
                        }
                    }
                } else if let Some(out) = leaf(o) {
                    output.last_mut().expect("root output").push(out);
                }
            }
            Work::Finish(mut out) => {
                let children = output.pop().expect("child output");
                out.insert("content".into(), JsValue::Array(children));
                output
                    .last_mut()
                    .expect("parent output")
                    .push(JsValue::Object(out));
            }
        }
    }
    output.pop().expect("root output")
}
fn message_metadata(value: Option<&JsValue>) -> Option<JsValue> {
    let o = record(value)?;
    let mut out = JsObject::new();
    copy_string(&mut out, o, "model");
    copy_boolean(&mut out, o, "synthetic");
    copy_boolean(&mut out, o, "transient");
    copy_string(&mut out, o, "transientId");
    copy_string(&mut out, o, "toolCallId");
    copy_boolean(&mut out, o, "compactReplacement");
    for key in ["compactSnapshotId", "purpose", "queueItemId"] {
        copy_string(&mut out, o, key);
    }
    if let Some(fork) = record(o.get("forkCarryover"))
        && let Some(session) = string(fork.get("sourceSessionId"))
    {
        let mut carry = object([("sourceSessionId", JsValue::String(session))]);
        copy_string(&mut carry, fork, "sourceTurnId");
        out.insert("forkCarryover".into(), JsValue::Object(carry));
    }
    (!out.is_empty()).then_some(JsValue::Object(out))
}
fn initial_messages(
    task: Option<&JsObject>,
    value: Option<&JsValue>,
    explicit: bool,
) -> Result<Vec<BoundarySnapshot>, FactoryPayloadError> {
    let mut messages = vec![];
    if let Some(JsValue::Array(values)) = value {
        for value in values {
            let Some(o) = value.as_object() else {
                continue;
            };
            let mut content = vec![];
            if let Some(s) = nonempty(o.get("content")) {
                content.push(text_block(s));
            }
            if let Some(JsValue::Array(a)) = o.get("content") {
                content.extend(blocks(a));
            }
            if let Some(JsValue::Array(a)) = o.get("images") {
                content.extend(blocks(a));
            }
            if !content.is_empty() {
                let mut message = object([
                    (
                        "role",
                        if is(o.get("role"), "assistant") {
                            "assistant"
                        } else {
                            "user"
                        }
                        .into(),
                    ),
                    ("content", JsValue::Array(content)),
                ]);
                if let Some(metadata) = message_metadata(o.get("metadata")) {
                    message.insert("metadata".into(), metadata);
                }
                messages.push(BoundarySnapshot::capture(&JsValue::Object(message)));
            }
        }
    }
    if explicit || !messages.is_empty() {
        return Ok(messages);
    }
    let prompt = nonempty(or(
        or(get(task, "prompt"), get(task, "instruction")),
        get(task, "description"),
    ));
    let text = match prompt {
        Some(prompt) => prompt,
        None => {
            let task = JsValue::Object(task.cloned().unwrap_or_default());
            stringify_json(&task)
                .map_err(|_| FactoryPayloadError::SourceTypeError {
                    message: "Task JSON.stringify failed.".into(),
                })?
                .ok_or_else(|| FactoryPayloadError::SourceTypeError {
                    message: "Task JSON.stringify returned undefined.".into(),
                })?
                .into()
        }
    };
    Ok(vec![BoundarySnapshot::capture(&JsValue::Object(object([
        ("role", "user".into()),
        ("content", JsValue::Array(vec![text_block(text)])),
    ])))])
}

pub fn decode(
    execute: &JsValue,
    process_cwd: &JsString,
) -> Result<FactoryPayload, FactoryPayloadError> {
    if matches!(execute, JsValue::Null | JsValue::Undefined) {
        let receiver = if matches!(execute, JsValue::Null) {
            "null"
        } else {
            "undefined"
        };
        return Err(FactoryPayloadError::SourceTypeError {
            message: format!("Cannot read properties of {receiver} (reading 'payload')").into(),
        });
    }
    let payload = record(execute.get("payload"));
    let agent = record(get(payload, "agent"));
    let task = record(get(payload, "task"));
    let context = record(get(payload, "contextOverride"));
    let host = record(get(payload, "hostModules"));
    let permission_input = record(get(payload, "permissionContext"));
    let session_id = required_string(or(execute.get("sessionId"), execute.get("operationId")))?;
    let turn_id = required_string(or(execute.get("turnId"), execute.get("operationId")))?;
    let raw_cwd = or(get(agent, "cwd"), get(payload, "cwd"));
    let cwd = if nullish(raw_cwd) {
        process_cwd.clone()
    } else {
        required_string(raw_cwd)?
    };
    let permission_mode = permission(get(permission_input, "mode"))
        .or_else(|| permission(get(agent, "permissionMode")))
        .unwrap_or(PermissionMode::Default);
    let mode = run_mode(or(get(agent, "runMode"), get(payload, "runMode")));
    let subagent_model = subagent(or(
        get(agent, "subagentModel"),
        get(payload, "subagentModel"),
    ))?;
    let provider_value = or(get(agent, "provider"), get(payload, "provider"));
    let provider = if nullish(provider_value) {
        "default".into()
    } else {
        required_string(provider_value)?
    };
    let model_value = or(get(agent, "model"), get(payload, "model"));
    let model = if nullish(model_value) {
        "default".into()
    } else {
        required_string(model_value)?
    };
    let config = FactoryRuntimeConfig {
        route: Route { provider, model },
        cwd,
        system_prompt: string(or(
            or(get(context, "systemPrompt"), get(agent, "systemPrompt")),
            get(payload, "systemPrompt"),
        )),
        append_system_prompt: nonempty(or(
            get(agent, "appendSystemPrompt"),
            get(payload, "appendSystemPrompt"),
        )),
        plan_mode_instructions: nonempty(or(
            get(agent, "planModeInstructions"),
            get(payload, "planModeInstructions"),
        )),
        runtime_context_surface: if is(get(agent, "runtimeContextSurface"), "user_message") {
            RuntimeContextSurface::UserMessage
        } else {
            RuntimeContextSurface::SystemPrompt
        },
        max_output_tokens: positive_integer(or(
            get(agent, "maxOutputTokens"),
            get(payload, "maxOutputTokens"),
        )),
        max_context_tokens: positive_integer(or(
            get(agent, "maxContextTokens"),
            get(payload, "maxContextTokens"),
        )),
        thinking: thinking(or(get(agent, "thinking"), get(payload, "thinking"))),
        tool_choice: tool_choice(or(get(agent, "toolChoice"), get(payload, "toolChoice"))),
        max_context_messages: positive_integer(or(
            get(agent, "maxContextMessages"),
            get(payload, "maxContextMessages"),
        )),
        stop_on_structured_output: boolean(or(
            get(agent, "stopOnStructuredOutput"),
            get(payload, "stopOnStructuredOutput"),
        )),
        json_self_correct: boolean(or(
            get(agent, "jsonSelfCorrect"),
            get(payload, "jsonSelfCorrect"),
        )),
        run_mode: mode,
        is_subagent: boolean(or(get(agent, "isSubagent"), get(payload, "isSubagent"))),
        subagent_model,
        permission_mode_before_plan: permission(get(agent, "permissionModeBeforePlan")),
        permission: FactoryPermissionContext {
            mode: permission_mode,
            can_prompt: boolean(get(permission_input, "canPrompt")) == Some(true),
            bypass_available: boolean(get(permission_input, "bypassAvailable")) == Some(true),
            additional_working_directories: directories(get(
                permission_input,
                "additionalWorkingDirectories",
            )),
            rules: rules(get(permission_input, "rules")),
        },
        metadata: merge_metadata([
            record(get(agent, "metadata")),
            record(get(payload, "executionContext")),
            record(get(context, "metadata")),
        ])?,
    };
    let override_tools = get(context, "tools");
    let tools = descriptors(
        if matches!(override_tools, None | Some(JsValue::Undefined)) {
            get(payload, "tools")
        } else {
            override_tools
        },
    );
    let override_messages = context.is_some_and(|o| o.contains_key("messages"));
    let model_override = model_override(or(
        get(agent, "modelOverride"),
        get(payload, "modelOverride"),
    ));
    let messages = initial_messages(
        task,
        if override_messages {
            get(context, "messages")
        } else {
            get(payload, "messages")
        },
        override_messages,
    )?;
    Ok(FactoryPayload {
        config,
        input: FactoryTurnInput {
            session_id,
            turn_id,
            messages,
            max_turns: positive_integer(or(get(agent, "maxTurns"), get(payload, "maxTurns"))),
            max_budget_usd: positive(get(payload, "maxBudgetUsd")),
            task_budget_usd: positive(get(payload, "taskBudgetUsd")),
            initial_task_budget_spent_usd: nonnegative(get(payload, "initialTaskBudgetSpentUsd")),
            base_permission_mode: permission(get(payload, "basePermissionMode")),
            allow_plan_mode_tools: boolean(get(payload, "allowPlanModeTools")) == Some(true),
            can_elicit: boolean(get(payload, "canElicit")) == Some(true),
            model_override,
        },
        tools,
        methods: methods(host),
        elicitation_available: boolean(get(
            record(get(payload, "interactionCapabilities")),
            "elicitationAvailable",
        )) == Some(true),
        seed_state: get(payload, "seedState").map(BoundarySnapshot::capture),
        model_state: get(payload, "modelState").map(BoundarySnapshot::capture),
    })
}
