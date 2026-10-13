//! Original request-boundary assembly. This module owns no provider, Session,
//! native graph, filesystem, lifecycle dispatcher or output sink.
use crate::ports::{
    CallControl, Captured, ContextPrepare, HostCall, HostPort, HostReply, ModelMetadata,
    PermissionMode, PortError, PortResult, Route, ToolDescriptor,
};
use crate::request::{
    PreparedRequestIntent, RequestAssemblyInput, RequestAssemblyOptions,
    RequestPreparationObserver, RequestPreparationState,
};
use crate::turn::{RunMode, TurnInput, TurnRuntimeConfig};
use crate::{MessageProjection, ProjectedMessage, ProjectionGap};
use pilotdeck_protocol::model::{
    BoundarySnapshot, ThinkingConfig, ThinkingMode, ToolChoice, ToolSchema,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const PLAN_REMINDER: &str = "Plan mode is active.\nRead first using read-only tools, then write or refine plan markdown only under `.pilotdeck/plans/`.\nDo not make implementation changes while planning.\nWhen the plan is ready for user review, call `exit_plan_mode` with the plan file path.";
fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    out
}
fn same(value: Option<&JsValue>, name: &str) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(name))
}
fn array(value: &JsValue) -> Option<&[JsValue]> {
    if let JsValue::Array(values) = value {
        Some(values)
    } else {
        None
    }
}
fn nullish(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined | JsValue::Null))
}
fn truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => false,
        Some(JsValue::Bool(b)) => *b,
        Some(JsValue::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(JsValue::String(s)) => !s.is_empty(),
        Some(JsValue::Object(_) | JsValue::Array(_)) => true,
    }
}
fn source_error(message: &str, value: &JsValue) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(value),
    }
}
fn gap(code: &str, message: &str, value: &JsValue) -> PortError {
    PortError::HostFailure {
        code: code.into(),
        message: message.into(),
        boundary: BoundarySnapshot::capture(value),
    }
}
fn wrong(method: &str) -> PortError {
    PortError::InvalidResponse {
        message: format!("Request assembly received the wrong {method} reply.").into(),
    }
}
// This is CachePlan.stableSerialize, not ordinary request JSON identity.
// Undefined is an absent serialization result at the root, an empty join slot
// in an array, and the literal `undefined` in an object template interpolation.
pub fn stable_serialize(value: &JsValue) -> PortResult<Option<String>> {
    enum Emit<'a> {
        Value(&'a JsValue, usize),
        Text(&'static str),
        Quoted(&'a JsString),
    }
    if value.is_undefined() {
        return Ok(None);
    }
    let mut out = String::new();
    let mut tasks = vec![Emit::Value(value, 0)];
    while let Some(task) = tasks.pop() {
        match task {
            Emit::Text(text) => out.push_str(text),
            Emit::Quoted(key) => {
                let quoted = stringify_json(&JsValue::String(key.clone()))
                    .map_err(|error| source_error(&error.to_string(), value))?
                    .expect("a string has JSON text");
                out.push_str(&quoted);
            }
            Emit::Value(item, depth) => {
                if depth >= 1024 {
                    return Err(gap(
                        "REQUEST_CACHE_NATIVE_RECURSION_PROFILE",
                        "Original recursive stableSerialize stack-limit behavior requires a verified native engine depth profile; this bounded wire serializer does not invent RangeError behavior.",
                        value,
                    ));
                }
                match item {
                    JsValue::Array(items) => {
                        out.push('[');
                        tasks.push(Emit::Text("]"));
                        for (index, child) in items.iter().enumerate().rev() {
                            if !child.is_undefined() {
                                tasks.push(Emit::Value(child, depth + 1));
                            }
                            if index > 0 {
                                tasks.push(Emit::Text(","));
                            }
                        }
                    }
                    JsValue::Object(object) => {
                        let mut entries = object.entries().iter().collect::<Vec<_>>();
                        entries.sort_by(|left, right| left.0.cmp(&right.0));
                        out.push('{');
                        tasks.push(Emit::Text("}"));
                        for (index, (key, child)) in entries.into_iter().enumerate().rev() {
                            if child.is_undefined() {
                                tasks.push(Emit::Text("undefined"));
                            } else {
                                tasks.push(Emit::Value(child, depth + 1));
                            }
                            tasks.push(Emit::Text(":"));
                            tasks.push(Emit::Quoted(key));
                            if index > 0 {
                                tasks.push(Emit::Text(","));
                            }
                        }
                    }
                    _ => {
                        let text = stringify_json(item)
                            .map_err(|error| source_error(&error.to_string(), value))?
                            .expect("nested undefined was handled by its container");
                        out.push_str(&text);
                    }
                }
            }
        }
    }
    Ok(Some(out))
}
fn cache_tools(prepared: &JsValue) -> PortResult<Vec<JsValue>> {
    match prepared.get("tools") {
        Some(JsValue::Array(items)) => Ok(items.clone()),
        Some(JsValue::String(string)) => {
            // JS string iteration groups a surrogate pair but preserves a lone
            // surrogate. Property spread, elsewhere, still iterates code units.
            let mut items = vec![];
            let mut at = 0;
            while at < string.units().len() {
                let unit = string.units()[at];
                let paired = (0xd800..=0xdbff).contains(&unit)
                    && string
                        .units()
                        .get(at + 1)
                        .is_some_and(|next| (0xdc00..=0xdfff).contains(next));
                let width = if paired { 2 } else { 1 };
                items.push(JsValue::String(JsString::from_utf16(
                    string.units()[at..at + width].to_vec(),
                )));
                at += width;
            }
            Ok(items)
        }
        _ => Err(source_error("input.tools is not iterable", prepared)),
    }
}
fn build_cache_plan(
    prepared: &JsValue,
    messages: &[JsValue],
    route: &Route,
) -> PortResult<JsValue> {
    let mut selected = vec![];
    for (index, message) in messages.iter().enumerate() {
        if !same(require_property(message, "role")?, "system") {
            selected.push(index);
        }
    }
    if selected.len() > 3 {
        selected.drain(..selected.len() - 3);
    }
    let tools = cache_tools(prepared)?;
    if tools.len() > 1 {
        return Err(gap(
            "REQUEST_CACHE_LOCALE_COLLATION_PROFILE",
            "Original tool ordering uses name.localeCompare followed by stableSerialize.localeCompare; multiple tools require an independently verified Node locale/ICU profile and a pure Rust collation implementation. No ASCII or UTF16 substitute was applied.",
            prepared,
        ));
    }
    let system = prepared.get("systemPrompt");
    let fingerprint_value = JsValue::Object(object([
        ("provider", JsValue::String(route.provider.clone())),
        ("model", JsValue::String(route.model.clone())),
        (
            "system",
            if nullish(system) {
                "".into()
            } else {
                system.cloned().expect("present")
            },
        ),
        ("tools", JsValue::Array(tools)),
        (
            "messages",
            JsValue::Array(
                selected
                    .iter()
                    .map(|index| messages[*index].clone())
                    .collect(),
            ),
        ),
    ]));
    // Stable serialization emits well-formed JSON string tokens (lone UTF16
    // surrogates are escaped), hence its text has an exact UTF8 hash encoding.
    let text = stable_serialize(&fingerprint_value)?.expect("fingerprint is an object");
    let fingerprint = format!("{:x}", Sha256::digest(text.as_bytes()));
    let breakpoints = JsValue::Array(
        selected
            .into_iter()
            .map(|index| JsValue::Number(index as f64))
            .collect(),
    );
    Ok(JsValue::Object(object([
        ("provider", JsValue::String(route.provider.clone())),
        ("model", JsValue::String(route.model.clone())),
        ("system", JsValue::Bool(truthy(system))),
        ("tools", JsValue::Bool(false)),
        ("messages", breakpoints),
        ("fingerprint", JsValue::String(fingerprint.into())),
        (
            "generation",
            prepared
                .get("cachePlan")
                .and_then(|cache| cache.get("generation"))
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
    ])))
}
fn require_property<'a>(value: &'a JsValue, key: &str) -> PortResult<Option<&'a JsValue>> {
    if matches!(value, JsValue::Null | JsValue::Undefined) {
        let receiver = if matches!(value, JsValue::Null) {
            "null"
        } else {
            "undefined"
        };
        return Err(source_error(
            &format!("Cannot read properties of {receiver} (reading '{key}')"),
            value,
        ));
    }
    Ok(value.get(key))
}
// Own-property spread echoes only. Native accessors/prototypes/functions are a
// separate negotiated binding profile, never emulated with mutable JSON state.
fn spread(value: Option<&JsValue>) -> JsObject {
    match value {
        Some(JsValue::Object(o)) => o.clone(),
        Some(JsValue::Array(a)) => {
            let mut out = JsObject::new();
            for (index, value) in a.iter().enumerate() {
                out.insert(index.to_string().into(), value.clone());
            }
            out
        }
        Some(JsValue::String(s)) => {
            let mut out = JsObject::new();
            for (index, unit) in s.units().iter().enumerate() {
                out.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
            out
        }
        _ => JsObject::new(),
    }
}
fn message_content(value: &JsValue) -> PortResult<&[JsValue]> {
    Ok(require_property(value, "content")?
        .and_then(array)
        .unwrap_or(&[]))
}
fn has_tool_call(value: &JsValue) -> PortResult<bool> {
    for block in message_content(value)? {
        if same(require_property(block, "type")?, "tool_call") {
            return Ok(true);
        }
    }
    Ok(false)
}
fn projected(value: JsValue) -> ProjectedMessage {
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(&value),
        normalized: MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    }
}
fn normalize(messages: &[ProjectedMessage]) -> PortResult<Vec<JsValue>> {
    let mut out: Vec<JsValue> = vec![];
    for raw in messages {
        let raw = raw.boundary.value();
        let content = message_content(raw)?;
        let mut message = spread(Some(raw));
        message.insert("content".into(), JsValue::Array(content.to_vec()));
        let message = JsValue::Object(message);
        if let Some(last) = out.last()
            && same(last.get("role"), "assistant")
            && same(message.get("role"), "assistant")
            && !has_tool_call(last)?
            && !has_tool_call(&message)?
        {
            let mut joined = message_content(last)?.to_vec();
            joined.extend_from_slice(message_content(&message)?);
            let first = last.get("metadata");
            let second = message.get("metadata");
            let metadata = if !truthy(first) && !truthy(second) {
                JsValue::Undefined
            } else {
                let mut metadata = spread(first);
                for (key, value) in spread(second).entries() {
                    metadata.insert(key.clone(), value.clone());
                }
                JsValue::Object(metadata)
            };
            *out.last_mut().expect("existing assistant") = JsValue::Object(object([
                ("role", "assistant".into()),
                ("content", JsValue::Array(joined)),
                ("metadata", metadata),
            ]));
            continue;
        }
        if same(message.get("role"), "assistant") && message_content(&message)?.is_empty() {
            continue;
        }
        out.push(message);
    }
    Ok(out)
}
fn clone_block(block: &JsValue) -> PortResult<JsValue> {
    let kind = require_property(block, "type")?;
    let mut cloned = spread(Some(block));
    if same(kind, "tool_result") {
        let content = block.get("content");
        let values = match content {
            Some(value) if array(value).is_some() => array(value).expect("array checked"),
            None | Some(JsValue::Undefined | JsValue::Null) => {
                let receiver = if matches!(content, Some(JsValue::Null)) {
                    "null"
                } else {
                    "undefined"
                };
                return Err(source_error(
                    &format!("Cannot read properties of {receiver} (reading 'map')"),
                    block,
                ));
            }
            _ => return Err(source_error("tr.content.map is not a function", block)),
        };
        cloned.insert(
            "content".into(),
            JsValue::Array(
                values
                    .iter()
                    .map(|item| JsValue::Object(spread(Some(item))))
                    .collect(),
            ),
        );
    } else if same(kind, "tool_call") {
        // structuredClone(input) detaches the admitted acyclic own-value echo.
        cloned.insert(
            "input".into(),
            block.get("input").cloned().unwrap_or(JsValue::Undefined),
        );
    }
    Ok(JsValue::Object(cloned))
}
fn clone_message(message: &JsValue) -> PortResult<JsValue> {
    let content = message_content(message)?;
    let mut out = spread(Some(message));
    out.insert(
        "content".into(),
        JsValue::Array(
            content
                .iter()
                .map(clone_block)
                .collect::<PortResult<Vec<_>>>()?,
        ),
    );
    Ok(JsValue::Object(out))
}
fn clone_messages(values: &[JsValue]) -> PortResult<Vec<JsValue>> {
    values.iter().map(clone_message).collect()
}
fn is_tool_result_only(message: &JsValue) -> PortResult<bool> {
    if !same(require_property(message, "role")?, "user") {
        return Ok(false);
    }
    let content = require_property(message, "content")?;
    let values = content
        .and_then(array)
        .ok_or_else(|| source_error("message.content.every is not a function", message))?;
    if values.is_empty() {
        return Ok(false);
    }
    for block in values {
        let kind = require_property(block, "type")?;
        if same(kind, "tool_result") || same(kind, "tool_result_reference") {
            continue;
        }
        if same(kind, "media_reference")
            && block
                .get("toolCallId")
                .and_then(JsValue::as_string)
                .is_some_and(|id| !id.is_empty())
        {
            continue;
        }
        return Ok(false);
    }
    Ok(true)
}
fn no_context(
    messages: &[ProjectedMessage],
    tools: &[ToolSchema],
    max: Option<f64>,
) -> PortResult<JsValue> {
    let mut cut = 0usize;
    if let Some(max) = max
        && messages.len() as f64 > max
    {
        let start = messages.len() as f64 - max;
        if start < messages.len() as f64 {
            // A fractional numeric property on this freshly normalized array
            // is missing. The original reads .role before slice can coerce it.
            if start.fract() != 0.0 {
                return Err(source_error(
                    "Cannot read properties of undefined (reading 'role')",
                    &JsValue::Undefined,
                ));
            }
            cut = start as usize;
            while cut < messages.len() && is_tool_result_only(messages[cut].boundary.value())? {
                if cut == 0 {
                    return Err(source_error(
                        "Cannot read properties of undefined (reading 'role')",
                        &JsValue::Undefined,
                    ));
                }
                cut -= 1;
            }
        } else {
            // Negative maxima, including -Infinity, produce an out-of-range
            // positive cut; Array.slice coerces and clamps it to length.
            cut = messages.len();
        }
    }
    Ok(JsValue::Object(object([
        (
            "messages",
            JsValue::Array(
                messages[cut..]
                    .iter()
                    .map(|m| m.boundary.value().clone())
                    .collect(),
            ),
        ),
        ("systemPromptParts", JsValue::Array(vec![])),
        (
            "tools",
            JsValue::Array(tools.iter().map(schema_value).collect()),
        ),
    ])))
}
fn schema_value(tool: &ToolSchema) -> JsValue {
    JsValue::Object(object([
        ("name", JsValue::String(tool.name.clone())),
        (
            "description",
            tool.description
                .clone()
                .map(JsValue::String)
                .unwrap_or(JsValue::Undefined),
        ),
        ("inputSchema", JsValue::Object(tool.input_schema.clone())),
    ]))
}
fn permission(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::Plan => "plan",
        PermissionMode::BypassPermissions => "bypassPermissions",
    }
}
fn run_mode(mode: Option<RunMode>) -> &'static str {
    match mode {
        Some(RunMode::Plan) => "plan",
        Some(RunMode::Ask) => "ask",
        _ => "agent",
    }
}
fn is_space(unit: u16) -> bool {
    matches!(unit,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}
fn addenda(values: [Option<&JsString>; 3]) -> Option<JsString> {
    let parts: Vec<_> = values
        .into_iter()
        .flatten()
        .filter(|s| s.units().iter().any(|u| !is_space(*u)))
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut out = vec![];
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.extend([10, 10]);
        }
        out.extend_from_slice(part.units());
    }
    Some(JsString::from_utf16(out))
}
fn visible_tools(
    state: &RequestPreparationState,
    config: &TurnRuntimeConfig,
    input: &TurnInput,
) -> Vec<ToolSchema> {
    let can_elicit = input.can_elicit && state.elicitation_available;
    // The original blocks by tool NAME across the whole catalog. An initial
    // duplicate descriptor with a different interaction flag cannot bypass it.
    let blocked: Vec<_> = if config.permission.can_prompt {
        vec![]
    } else {
        state
            .tools
            .iter()
            .filter(|tool| {
                (tool.name.equals_str("enter_plan_mode") || tool.requires_user_interaction)
                    && !(can_elicit && tool.name.equals_str("ask_user_question"))
            })
            .map(|tool| tool.name.clone())
            .collect()
    };
    state.tools.iter().filter(|tool|!blocked.contains(&tool.name))
        .filter(|tool|input.allow_plan_mode_tools||(!tool.name.equals_str("enter_plan_mode")&&!tool.name.equals_str("exit_plan_mode")))
        .filter(|tool|config.run_mode!=Some(RunMode::Ask)||ask_allowed(tool))
        .map(|tool|{if config.run_mode==Some(RunMode::Ask)&&tool.name.equals_str("agent"){return ask_agent_schema(tool.name.clone());}
            let mut description=tool.description.clone();if config.run_mode==Some(RunMode::Ask){let suffix=if tool.name.equals_str("bash"){Some("\n\n[ASK MODE] READ-ONLY commands only. Write/modify/delete commands will be rejected.")}else if tool.name.equals_str("execute_code"){Some("\n\n[ASK MODE] READ-ONLY Python scripts only. Scripts that call write_file, edit_file, or non-read-only bash commands will be rejected.")}else{None};if let Some(suffix)=suffix{let mut units=description.units().to_vec();units.extend(suffix.encode_utf16());description=JsString::from_utf16(units);}}
            ToolSchema{name:tool.name.clone(),description:Some(description),input_schema:tool.input_schema.clone()}}).collect()
}
fn ask_allowed(tool: &ToolDescriptor) -> bool {
    if tool.kind.equals_str("mcp") {
        return tool.read_only;
    }
    [
        "read_file",
        "get_current_time",
        "grep",
        "glob",
        "web_search",
        "web_fetch",
        "ask_user_question",
        "read_skill",
        "structured_output",
        "agent",
        "execute_code",
        "bash",
        "task_list",
        "task_output",
        "task_wait",
    ]
    .iter()
    .any(|name| tool.name.equals_str(name))
}
fn ask_agent_schema(name: JsString) -> ToolSchema {
    let description=["Launch a read-only subagent for investigation, planning, or verification.","","In ask mode, subagents inherit ask mode and the same permission setting. Only read-only subagent types are available; 'general-purpose' is treated as 'explore'.","","Provide:","- `description`: a short 3-5 word label for the task.","- `prompt`: the full directive for the subagent. Include goal, context, constraints, and what good output looks like. The subagent can only read and search; it cannot modify files.","- `subagent_type` (optional): 'explore', 'plan', or 'verify'. Defaults to 'explore'.","","Available subagent types:","- explore: Read-only exploration subagent. Inspects files, runs grep/glob, and may run safe shell commands. Cannot edit files. Tools: read_file, grep, glob, bash.","- plan: Read-only planning subagent. Inspects code via read/grep/glob and produces a step-by-step plan. Tools: read_file, grep, glob.","- verify: Verification subagent. Inspects generated artifacts (images, HTML, PDFs) for correctness. Can read files, run shell commands, and search code, but cannot modify files. Tools: read_file, grep, glob, bash.","","The subagent returns one structured report with these sections: `Scope`, `Result`, `Key files`, `Files changed`, and `Issues`."].join("\n");
    let string_property = |description: &str| {
        JsValue::Object(object([
            ("type", "string".into()),
            ("description", description.into()),
        ]))
    };
    ToolSchema {
        name,
        description: Some(description.into()),
        input_schema: object([
            ("type", "object".into()),
            (
                "required",
                JsValue::Array(vec!["description".into(), "prompt".into()]),
            ),
            ("additionalProperties", JsValue::Bool(false)),
            (
                "properties",
                JsValue::Object(object([
                    (
                        "description",
                        string_property(
                            "Short 3-5 word task summary used to label the subagent run.",
                        ),
                    ),
                    (
                        "prompt",
                        string_property(
                            "Detailed directive for the subagent. Include the goal, relevant context, constraints, and desired output. The subagent can only read and search; it cannot modify files.",
                        ),
                    ),
                    (
                        "subagent_type",
                        string_property(
                            "Subagent preset. In ask mode only 'explore', 'plan', and 'verify' are available. Defaults to 'explore'.",
                        ),
                    ),
                    (
                        "subagentType",
                        string_property(
                            "Deprecated legacy alias for subagent_type. Prefer subagent_type.",
                        ),
                    ),
                ])),
            ),
        ]),
    }
}
// Source Map keys concatenate UTF16 provider, one NUL, then model. These
// compatibility keys intentionally preserve collisions; route identity checks
// and subagent baseline comparison remain exact pairs.
fn route_key(route: &Route) -> JsString {
    JsString::from_utf16(
        route
            .provider
            .units()
            .iter()
            .copied()
            .chain(std::iter::once(0))
            .chain(route.model.units().iter().copied())
            .collect(),
    )
}
fn effective_metadata(state: &RequestPreparationState) -> Vec<&Captured<ModelMetadata>> {
    // Read projection only: latest matching key replaces the first insertion
    // slot, as snapshots.set does. No host/session state is mutated here.
    let mut positions: HashMap<JsString, usize> = HashMap::new();
    let mut snapshots = Vec::with_capacity(state.metadata.len());
    for snapshot in &state.metadata {
        let key = route_key(&snapshot.view.route);
        if let Some(index) = positions.get(&key) {
            snapshots[*index] = snapshot;
        } else {
            positions.insert(key, snapshots.len());
            snapshots.push(snapshot);
        }
    }
    snapshots
}
pub fn compaction_limits(
    state: &RequestPreparationState,
    config: &TurnRuntimeConfig,
    route: &Route,
    pre_route: bool,
) -> (f64, f64) {
    let key = route_key(route);
    let cap = state
        .token_caps
        .iter()
        .rev()
        .find(|cap| route_key(&cap.route) == key);
    let baseline = config
        .subagent_model
        .as_ref()
        .filter(|baseline| config.is_subagent == Some(true) && baseline.route == *route);
    let config_active = !(config.is_subagent == Some(true) && config.subagent_model.is_some());
    let metadata = effective_metadata(state);
    let metadata = metadata
        .iter()
        .find(|metadata| route_key(&metadata.view.route) == key);
    let max_context_tokens = if pre_route && !config_active {
        1_000_000.0
    } else {
        cap.and_then(|cap| cap.max_context_tokens)
            .or(baseline.and_then(|baseline| baseline.max_context_tokens))
            .or(if config_active {
                config.max_context_tokens
            } else {
                None
            })
            .or(metadata.and_then(|metadata| metadata.view.max_context_tokens))
            .or(metadata
                .and_then(|metadata| metadata.view.token_limits.as_ref())
                .map(|limits| limits.max_context_tokens))
            .unwrap_or(1_000_000.0)
    };
    let requested = cap
        .and_then(|cap| {
            cap.attempt_max_output_tokens
                .or(cap.requested_max_output_tokens)
                .or(cap.session_max_output_tokens)
        })
        .or(baseline.and_then(|baseline| baseline.max_output_tokens))
        .or(if config_active {
            config.max_output_tokens
        } else {
            None
        });
    let mut candidates = [requested, cap.and_then(|cap| cap.hard_max_output_tokens)]
        .into_iter()
        .flatten()
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    if !candidates.is_empty()
        && let Some(limit) = metadata
            .and_then(|metadata| match &metadata.view.token_limits {
                Some(limits) => limits.max_output_tokens,
                None => metadata.view.max_output_tokens,
            })
            .filter(|value| value.is_finite() && *value > 0.0)
    {
        candidates.push(limit);
    }
    (
        max_context_tokens,
        candidates
            .into_iter()
            .map(f64::floor)
            .reduce(f64::min)
            .unwrap_or(0.0),
    )
}

fn output_cap(
    state: &RequestPreparationState,
    config: &TurnRuntimeConfig,
    route: &Route,
    metadata: &[&Captured<ModelMetadata>],
) -> Option<f64> {
    let key = route_key(route);
    let cap = state
        .token_caps
        .iter()
        .rev()
        .find(|cap| route_key(&cap.route) == key);
    let subagent = config.subagent_model.as_ref();
    let subagent_active = config.is_subagent == Some(true) && subagent.is_some();
    let baseline = if config.is_subagent == Some(true) {
        subagent
            .filter(|model| model.route == *route)
            .and_then(|model| model.max_output_tokens)
    } else {
        None
    };
    let requested = cap
        .and_then(|cap| {
            cap.attempt_max_output_tokens
                .or(cap.requested_max_output_tokens)
                .or(cap.session_max_output_tokens)
        })
        .or(baseline)
        .or(if subagent_active {
            None
        } else {
            config.max_output_tokens
        });
    let mut candidates: Vec<_> = [requested, cap.and_then(|cap| cap.hard_max_output_tokens)]
        .into_iter()
        .flatten()
        .filter(|n| n.is_finite() && *n > 0.0)
        .collect();
    if !candidates.is_empty() {
        let metadata = metadata
            .iter()
            .find(|metadata| route_key(&metadata.view.route) == key);
        let model_max = metadata.and_then(|metadata| {
            if let Some(limits) = &metadata.view.token_limits {
                limits.max_output_tokens
            } else {
                metadata.view.max_output_tokens
            }
        });
        if let Some(max) = model_max.filter(|n| n.is_finite() && *n > 0.0) {
            candidates.push(max);
        }
    }
    candidates
        .into_iter()
        .map(f64::floor)
        .reduce(f64::min)
        .or(config.max_output_tokens)
}
fn choice_value(choice: Option<&ToolChoice>) -> JsValue {
    match choice {
        None => JsValue::Undefined,
        Some(ToolChoice::Auto) => "auto".into(),
        Some(ToolChoice::None) => "none".into(),
        Some(ToolChoice::Required) => "required".into(),
        Some(ToolChoice::Tool { name }) => JsValue::Object(object([
            ("type", "tool".into()),
            ("name", JsValue::String(name.clone())),
        ])),
    }
}
fn thinking_value(thinking: Option<&ThinkingConfig>) -> JsValue {
    let Some(thinking) = thinking else {
        return JsValue::Undefined;
    };
    let mut out = object([("enabled", JsValue::Bool(thinking.enabled))]);
    if let Some(mode) = thinking.mode {
        let mode = match mode {
            ThinkingMode::Default => "default",
            ThinkingMode::Off => "off",
            ThinkingMode::Minimal => "minimal",
            ThinkingMode::Low => "low",
            ThinkingMode::Medium => "medium",
            ThinkingMode::High => "high",
            ThinkingMode::XHigh => "xhigh",
            ThinkingMode::Max => "max",
        };
        out.insert("mode".into(), mode.into());
    }
    if let Some(budget) = thinking.budget_tokens {
        out.insert("budgetTokens".into(), JsValue::Number(budget));
    }
    if let Some(preserve) = thinking.preserve {
        out.insert("preserve".into(), JsValue::Bool(preserve));
    }
    if let Some(split) = thinking.split_reasoning {
        out.insert("splitReasoning".into(), JsValue::Bool(split));
    }
    JsValue::Object(out)
}
fn prepared_messages(prepared: &JsValue) -> PortResult<Vec<JsValue>> {
    let messages = require_property(prepared, "messages")?;
    let values = match messages {
        Some(value) if array(value).is_some() => array(value).expect("array checked"),
        None | Some(JsValue::Undefined | JsValue::Null) => {
            let receiver = if matches!(messages, Some(JsValue::Null)) {
                "null"
            } else {
                "undefined"
            };
            return Err(source_error(
                &format!("Cannot read properties of {receiver} (reading 'map')"),
                prepared,
            ));
        }
        _ => return Err(source_error("messages.map is not a function", prepared)),
    };
    let cloned = clone_messages(values)?;
    for message in &cloned {
        for block in message_content(message)? {
            if same(require_property(block, "type")?, "media_reference") {
                return Err(gap(
                    "REQUEST_MEDIA_REFERENCE_MATERIALIZATION_PROFILE",
                    "Top-level media_reference requires the authorized host filesystem materializer; no media was dropped or read.",
                    block,
                ));
            }
        }
    }
    Ok(cloned)
}

/// Coordinator integrates the actual trait implementation. No ModelPrepare,
/// stream, lifecycle dispatch or instructions_loaded event occurs here.
pub fn assemble(
    state: &mut RequestPreparationState,
    host: &mut dyn HostPort,
    control: &CallControl,
    config: &TurnRuntimeConfig,
    input: &TurnInput,
    messages: &[ProjectedMessage],
    observer: &mut dyn RequestPreparationObserver,
) -> PortResult<PreparedRequestIntent> {
    assemble_with_options(
        state,
        host,
        RequestAssemblyInput {
            control,
            config,
            input,
            messages,
        },
        RequestAssemblyOptions::default(),
        observer,
    )
}

pub fn context_preparation(
    state: &RequestPreparationState,
    parameters: RequestAssemblyInput<'_>,
    preview_only: Option<bool>,
) -> PortResult<ContextPrepare> {
    let RequestAssemblyInput {
        config,
        input,
        messages,
        ..
    } = parameters;
    let route = input
        .model_override
        .as_ref()
        .map_or_else(|| config.route.clone(), |model| model.route.clone());
    let metadata = effective_metadata(state);
    let protocol = metadata.iter().find_map(|metadata| {
        if metadata.view.route.provider == route.provider {
            metadata.view.protocol
        } else {
            None
        }
    });
    let supports_prompt_cache = metadata
        .iter()
        .find(|metadata| route_key(&metadata.view.route) == route_key(&route))
        .and_then(|metadata| metadata.view.supports_prompt_cache);
    let normalized = normalize(messages)?;
    let messages = clone_messages(&normalized)?
        .into_iter()
        .map(projected)
        .collect();
    let tools = visible_tools(state, config, input);
    Ok(ContextPrepare {
        preview_only,
        session_id: input.session_id.clone(),
        turn_id: input.turn_id.clone(),
        step_id: None,
        cwd: config.cwd.clone(),
        runtime_context_surface: Some(config.runtime_context_surface),
        route: route.clone(),
        protocol,
        supports_prompt_cache,
        permission_mode: permission(config.permission.mode).into(),
        run_mode: Some(run_mode(config.run_mode).into()),
        additional_working_directories: config.permission.additional_working_directories.clone(),
        messages,
        tools,
        custom_system_prompt: config.system_prompt.clone(),
        append_system_prompt: addenda([
            config.append_system_prompt.as_ref(),
            if config.permission.mode == PermissionMode::Plan {
                config.plan_mode_instructions.as_ref()
            } else {
                None
            },
            state.plan_todo_addendum.as_ref(),
        ]),
        max_messages: config.max_context_messages,
    })
}

pub fn assemble_with_options(
    state: &mut RequestPreparationState,
    host: &mut dyn HostPort,
    parameters: RequestAssemblyInput<'_>,
    options: RequestAssemblyOptions,
    observer: &mut dyn RequestPreparationObserver,
) -> PortResult<PreparedRequestIntent> {
    let RequestAssemblyInput {
        control,
        config,
        input,
        messages,
    } = parameters;
    control.check()?;
    if host.advertised("capability", "list_tools") {
        let HostReply::Tools(tools) = host.call(control, HostCall::ListTools)? else {
            return Err(wrong("list_tools"));
        };
        state.tools = tools.into_iter().map(|tool| tool.view).collect();
        control.check()?;
    }
    let route = input
        .model_override
        .as_ref()
        .map_or_else(|| config.route.clone(), |model| model.route.clone());
    let metadata = effective_metadata(state);
    let prepare = context_preparation(
        state,
        RequestAssemblyInput {
            control,
            config,
            input,
            messages,
        },
        options.preview_only,
    )?;
    let prepared = if host.advertised("context", "prepare_for_model") {
        let response = host.call(control, HostCall::PrepareContext(Box::new(prepare)))?;
        match response {
            HostReply::Context(context) => context.boundary.value().clone(),
            HostReply::ContextBoundaryOnly(boundary) => boundary.value().clone(),
            _ => return Err(wrong("prepare_for_model")),
        }
    } else {
        no_context(&prepare.messages, &prepare.tools, prepare.max_messages)?
    };
    // InstructionsLoaded uses the PREPARED prompt's truthiness, not the final
    // config fallback. Its actual event/dispatch owner is the surrounding loop.
    let mut has_system_prompt = false;
    if options.emit_instruction_events {
        has_system_prompt = truthy(require_property(&prepared, "systemPrompt")?);
        observer.instructions_loaded(has_system_prompt)?;
    }
    control.check()?;
    let mut messages = prepared_messages(&prepared)?;
    if config.permission.mode == PermissionMode::Plan {
        messages.push(JsValue::Object(object([
            ("role", "user".into()),
            (
                "content",
                JsValue::Array(vec![JsValue::Object(object([
                    ("type", "text".into()),
                    ("text", PLAN_REMINDER.into()),
                ]))]),
            ),
            (
                "metadata",
                JsValue::Object(object([
                    ("synthetic", JsValue::Bool(true)),
                    ("purpose", "plan_mode_reminder".into()),
                ])),
            ),
        ])));
    }
    let cache_plan = if truthy(prepared.get("cachePlan")) {
        build_cache_plan(&prepared, &messages, &route)?
    } else {
        JsValue::Undefined
    };
    if !options.emit_instruction_events {
        // Diagnostic only, after source finalization failures; suppressed
        // notification must not read a null prepared.systemPrompt first.
        has_system_prompt = truthy(prepared.get("systemPrompt"));
    }
    let system = prepared.get("systemPrompt");
    let system = if nullish(system) {
        config
            .system_prompt
            .clone()
            .map(JsValue::String)
            .unwrap_or(JsValue::Undefined)
    } else {
        system.cloned().expect("nonnull present")
    };
    let max_output = output_cap(state, config, &route, &metadata)
        .map(JsValue::Number)
        .unwrap_or(JsValue::Undefined);
    let model_override = input.model_override.as_ref();
    let thinking = model_override
        .and_then(|model| model.thinking.as_ref())
        .or(config.thinking.as_ref());
    let request = JsValue::Object(object([
        ("provider", JsValue::String(route.provider.clone())),
        ("model", JsValue::String(route.model.clone())),
        ("messages", JsValue::Array(messages)),
        ("systemPrompt", system),
        (
            "tools",
            prepared.get("tools").cloned().unwrap_or(JsValue::Undefined),
        ),
        ("toolChoice", choice_value(config.tool_choice.as_ref())),
        ("maxOutputTokens", max_output),
        (
            "speed",
            model_override
                .and_then(|model| model.speed)
                .map(JsValue::Number)
                .unwrap_or(JsValue::Undefined),
        ),
        ("thinking", thinking_value(thinking)),
        ("stream", JsValue::Bool(true)),
        (
            "metadata",
            config
                .metadata
                .clone()
                .map(JsValue::Object)
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "cacheBreakpoints",
            if !cache_plan.is_undefined() {
                cache_plan
                    .get("messages")
                    .cloned()
                    .expect("cache plan owns messages")
            } else if config.permission.mode == PermissionMode::Plan {
                JsValue::Undefined
            } else {
                prepared
                    .get("cacheBreakpoints")
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            },
        ),
        ("cachePlan", cache_plan),
    ]));
    Ok(PreparedRequestIntent {
        route,
        request: BoundarySnapshot::capture(&request),
        diagnostics: vec![],
        has_system_prompt,
    })
}
